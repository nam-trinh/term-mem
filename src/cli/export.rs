//! `tmem export` and `tmem import`.
//!
//! docs/cli.md calls export "the concrete form of the mission's ownership
//! promise", and docs/plan.md explains why it stops being a nicety in this
//! phase: with encryption on, the archive is no longer something the user can
//! reach with `grep` and `sqlite3`, so a guaranteed open-format export *is* the
//! ownership promise rather than a convenience on top of it.
//!
//! The JSON form is the interchange format: one record per line, the same shape
//! `--json` produces everywhere else, and it is what `import` reads. The
//! markdown form is for people and is deliberately not re-importable — a format
//! that is both pretty and lossless is neither.

use crate::cli::BrowseArgs;
use crate::db::{self, queries};
use crate::output::{EXIT_EMPTY, EXIT_OK};
use crate::paths;
use anyhow::{Context, Result};
use std::io::{BufRead, Write};
use std::path::Path;

/// `tmem export --markdown | head` closes the pipe early, and that is the user
/// getting what they asked for rather than a failure to report.
fn ignore_broken_pipe(r: std::io::Result<()>) -> Result<bool> {
    match r {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Json,
    Markdown,
}

pub fn export(format: Format, args: &BrowseArgs) -> Result<i32> {
    let conn = db::open(&paths::db_path()?)?;
    let mut filter = args.to_filter()?;
    // An export is an archive, not a result page. `-n` still works if the user
    // asks for it explicitly, but the default 20 would quietly hand someone a
    // twentieth of their history and call it a backup.
    if !args.limit_was_given() {
        filter.limit = None;
    }
    let rows = queries::list(&conn, &filter)?;
    if rows.is_empty() {
        eprintln!("tmem: nothing to export");
        return Ok(EXIT_EMPTY);
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let written = write_all(&mut out, format, &rows);
    if !ignore_broken_pipe(written)? {
        return Ok(EXIT_OK);
    }
    if !ignore_broken_pipe(out.flush())? {
        return Ok(EXIT_OK);
    }
    Ok(EXIT_OK)
}

fn write_all(
    out: &mut impl Write,
    format: Format,
    rows: &[queries::Exchange],
) -> std::io::Result<()> {
    match format {
        Format::Json => {
            for ex in rows {
                let line = serde_json::to_string(ex).expect("an exchange serialises");
                writeln!(out, "{line}")?;
            }
        }
        Format::Markdown => {
            for ex in rows {
                writeln!(out, "## {}", crate::output::fmt_datetime(ex.ts))?;
                writeln!(out)?;
                writeln!(out, "- id: `{}`", ex.id)?;
                writeln!(out, "- cwd: `{}`", ex.cwd)?;
                if let Some(r) = &ex.repo {
                    writeln!(
                        out,
                        "- repo: `{r}`{}",
                        ex.git_branch
                            .as_deref()
                            .map(|b| format!(" (`{b}`)"))
                            .unwrap_or_default()
                    )?;
                }
                writeln!(
                    out,
                    "- via: `{}`{}",
                    ex.assistant,
                    ex.model
                        .as_deref()
                        .map(|m| format!(" / `{m}`"))
                        .unwrap_or_default()
                )?;
                if ex.redacted {
                    writeln!(out, "- note: contains redacted content")?;
                }
                writeln!(out, "\n### Prompt\n\n{}", ex.prompt.trim())?;
                if !ex.response.trim().is_empty() {
                    writeln!(out, "\n### Response\n\n{}", ex.response.trim())?;
                }
                if !ex.commands.is_empty() {
                    writeln!(out, "\n### Commands\n")?;
                    for c in &ex.commands {
                        writeln!(out, "```console\n{c}\n```")?;
                    }
                }
                writeln!(out, "\n---\n")?;
            }
        }
    }
    Ok(())
}

pub fn import(path: &Path) -> Result<i32> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut conn = db::open(&paths::db_path()?)?;
    // Imported text is redacted like anything else on the way in. An export may
    // predate a rule, and this is the one other door into the database.
    let redactor = crate::redact::Redactor::load()?;

    let mut inserted = 0usize;
    let mut existing = 0usize;
    let mut forgotten = 0usize;
    let mut redacted = 0usize;
    let mut bad = 0usize;

    let tx = conn.transaction()?;
    for (n, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let mut ex: queries::Exchange = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Loud, and keep going: one bad line must not cost the rest of
                // someone's history. The count is reported at the end.
                eprintln!("tmem: {}:{}: skipped — {e}", path.display(), n + 1);
                bad += 1;
                continue;
            }
        };
        let mut report = redactor.scrub(&mut ex.prompt);
        report.merge(&redactor.scrub(&mut ex.response));
        for c in &mut ex.commands {
            report.merge(&redactor.scrub(c));
        }
        if !report.is_empty() {
            ex.redacted = true;
            redacted += 1;
        }
        match queries::import_exchange(&tx, &ex)? {
            queries::Imported::Inserted => inserted += 1,
            queries::Imported::AlreadyPresent => existing += 1,
            queries::Imported::Forgotten => forgotten += 1,
        }
    }
    tx.commit()?;

    println!("imported {inserted} exchange(s)");
    if existing > 0 {
        println!("  {existing} already present");
    }
    if forgotten > 0 {
        // Import is an explicit act, but so was `forget`, and only one of them
        // is irreversible. The tombstone wins and says so.
        println!("  {forgotten} left out because you forgot them — `tmem status` counts these");
    }
    if redacted > 0 {
        println!("  {redacted} redacted on the way in");
    }
    if bad > 0 {
        println!("  {bad} line(s) could not be read (see above)");
    }
    Ok(if inserted == 0 && bad > 0 {
        crate::output::EXIT_ERROR
    } else {
        EXIT_OK
    })
}
