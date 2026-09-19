//! Ingest: run an adapter over a transcript and write the result idempotently.
//!
//! Both triggers — the `Stop` hook and (from Phase 6) the file watcher —
//! converge here, which is what removes the double-write problem: one parser,
//! one key, two ways of being told to run.

pub mod adapters;
pub mod queue;

use adapters::{Adapter, ParseReport, ParsedExchange};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IngestStats {
    pub files_seen: usize,
    pub files_parsed: usize,
    pub files_skipped_unchanged: usize,
    pub files_ignored: usize,
    /// Exchanges the transcript still holds but the user has deleted.
    pub skipped_forgotten: usize,
    pub inserted: usize,
    pub updated: usize,
    pub report: ParseReport,
}

impl IngestStats {
    fn absorb(&mut self, r: ParseReport) {
        self.report.records_total += r.records_total;
        self.report.records_unparsable += r.records_unparsable;
        self.report.records_unknown_type += r.records_unknown_type;
        self.report.prompts_found += r.prompts_found;
        self.report.prompts_without_response += r.prompts_without_response;
        self.report.prompts_unusable += r.prompts_unusable;
        self.report.api_errors_skipped += r.api_errors_skipped;
        self.report.sidechain_records += r.sidechain_records;
        self.report.orphaned_records += r.orphaned_records;
        self.report.orphaned_chars += r.orphaned_chars;
        for t in r.unknown_types {
            if !self.report.unknown_types.contains(&t) {
                self.report.unknown_types.push(t);
            }
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Ingest one transcript file.
///
/// `force` bypasses the watermark, which is otherwise a change detector: if the
/// file has neither grown nor been touched since the last run, there is nothing
/// to do. It is deliberately not a byte offset — see `Adapter::parse`.
pub fn ingest_file(
    conn: &mut Connection,
    adapter: &dyn Adapter,
    path: &Path,
    ignores: &[PathBuf],
    force: bool,
) -> Result<IngestStats> {
    let mut stats = IngestStats {
        files_seen: 1,
        ..Default::default()
    };
    let meta = std::fs::metadata(path)
        .with_context(|| format!("reading transcript {}", path.display()))?;
    let bytes = meta.len() as i64;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let path_s = path.to_string_lossy().into_owned();

    if !force {
        let prev: Option<(i64, i64)> = conn
            .query_row(
                "SELECT bytes, mtime_ms FROM watermarks WHERE assistant = ?1 AND source_path = ?2",
                params![adapter.name(), &path_s],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((pb, pm)) = prev {
            if pb == bytes && pm == mtime_ms {
                stats.files_skipped_unchanged = 1;
                return Ok(stats);
            }
        }
    }

    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading transcript {}", path.display()))?;
    let (exchanges, report) = adapter.parse(&source, &path_s)?;
    stats.files_parsed = 1;
    stats.absorb(report);

    let mut session_id = None;
    // One `cwd` per transcript is the norm, and resolving a repo means walking
    // to the filesystem root looking for `.git`. Doing that per exchange is
    // hundreds of stat calls for one answer — and on a path that does not exist
    // locally it can block for milliseconds each time.
    let mut repos: HashMap<String, Option<String>> = HashMap::new();
    let tx = conn.transaction()?;
    for ex in &exchanges {
        if is_ignored(Path::new(&ex.cwd), ignores) {
            stats.files_ignored += 1;
            continue;
        }
        session_id.get_or_insert_with(|| ex.session_id.clone());
        let repo = repos
            .entry(ex.cwd.clone())
            .or_insert_with(|| resolve_repo(Path::new(&ex.cwd)))
            .clone();
        match write_exchange(&tx, adapter.name(), ex, repo)? {
            Written::Inserted => stats.inserted += 1,
            Written::Updated => stats.updated += 1,
            Written::Unchanged => {}
            Written::Forgotten => stats.skipped_forgotten += 1,
        }
    }
    tx.execute(
        "INSERT INTO watermarks (assistant, source_path, session_id, bytes, mtime_ms, exchanges, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(assistant, source_path) DO UPDATE SET \
           session_id = excluded.session_id, bytes = excluded.bytes, \
           mtime_ms = excluded.mtime_ms, exchanges = excluded.exchanges, \
           updated_at = excluded.updated_at",
        params![
            adapter.name(),
            &path_s,
            session_id,
            bytes,
            mtime_ms,
            exchanges.len() as i64,
            now_ms()
        ],
    )?;
    tx.commit()?;
    Ok(stats)
}

enum Written {
    Inserted,
    Updated,
    Unchanged,
    Forgotten,
}

/// The idempotency point. Keyed on the adapter-declared
/// `(assistant, session_id, source_key)`, so a re-run over the same transcript
/// is a no-op and a mid-turn re-run completes the row it already wrote instead
/// of adding a second one.
fn write_exchange(
    tx: &rusqlite::Transaction,
    assistant: &str,
    ex: &ParsedExchange,
    repo: Option<String>,
) -> Result<Written> {
    // The transcript outlives the row, so a forgotten exchange would otherwise
    // come straight back on the next ingest of the same file.
    let tombstoned: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM forgotten \
         WHERE assistant = ?1 AND session_id = ?2 AND source_key = ?3)",
        params![assistant, &ex.session_id, &ex.source_key],
        |r| r.get::<_, i64>(0),
    )? != 0;
    if tombstoned {
        return Ok(Written::Forgotten);
    }

    // Everything the row stores comes back, and everything is compared. The
    // temptation here is to compare something cheap and skip the rewrite, and
    // it has now been wrong twice:
    //
    //   * Phase 1 compared response text alone, so an exchange that gained a
    //     tool call kept neither the command nor a way to recover it.
    //   * Phase 2 compared response text plus derived *counts*, which misses a
    //     changed prompt entirely — and Phase 2 is the release that changed how
    //     prompts are extracted, so every row written by a Phase 1 binary would
    //     have kept its unstripped prompt forever.
    //   * The first fix for that added the prompt and the command text, and a
    //     review pointed out the comment then claimed more than the code did:
    //     `repo` was still not compared, so an exchange captured before
    //     `git init` kept `repo = NULL` while later turns in the same session
    //     got the repo — `--repo` returning half a session, unrepairable.
    //
    // So: every column the row stores. The parse already happened and the row
    // is already in hand, and each round of guessing which subset is safe has
    // cost more than the comparison ever would.
    type Existing = (
        String,
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    let existing: Option<Existing> = tx
        .query_row(
            "SELECT id, prompt, response, commands_text, ts, cwd, repo, git_branch, model, \
             thread_id FROM exchanges \
             WHERE assistant = ?1 AND session_id = ?2 AND source_key = ?3",
            params![assistant, &ex.session_id, &ex.source_key],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .optional()?;

    if let Some((
        id,
        prev_prompt,
        prev_response,
        prev_commands,
        prev_ts,
        prev_cwd,
        prev_repo,
        prev_branch,
        prev_model,
        prev_thread,
    )) = existing
    {
        let prev_files: Vec<String> = tx
            .prepare_cached("SELECT path FROM file_refs WHERE exchange_id = ?1 ORDER BY seq")?
            .query_map(params![&id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        if prev_prompt == ex.prompt
            && prev_response == ex.response
            && prev_commands == commands_text(ex)
            && prev_ts == ex.ts_ms
            && prev_cwd == ex.cwd
            && prev_repo == repo
            && prev_branch == ex.git_branch
            && prev_model == ex.model
            && prev_thread == ex.thread_id
            && prev_files.len() == ex.files.len()
            && prev_files.iter().zip(&ex.files).all(|(p, f)| *p == f.path)
        {
            return Ok(Written::Unchanged);
        }
        tx.execute(
            "UPDATE exchanges SET ts = ?2, cwd = ?3, repo = ?4, git_branch = ?5, model = ?6, \
             prompt = ?7, response = ?8, thread_id = ?9, commands_text = ?10 WHERE id = ?1",
            params![
                &id,
                ex.ts_ms,
                &ex.cwd,
                repo,
                &ex.git_branch,
                &ex.model,
                &ex.prompt,
                &ex.response,
                &ex.thread_id,
                commands_text(ex)
            ],
        )?;
        tx.execute("DELETE FROM commands  WHERE exchange_id = ?1", params![&id])?;
        tx.execute("DELETE FROM file_refs WHERE exchange_id = ?1", params![&id])?;
        write_derived(tx, &id, ex)?;
        return Ok(Written::Updated);
    }

    let id = ulid::Ulid::from_parts(ex.ts_ms.max(0) as u64, rand_u128()).to_string();
    tx.execute(
        "INSERT INTO exchanges (id, assistant, session_id, thread_id, source_key, ts, cwd, \
         repo, git_branch, model, prompt, response, commands_text, redacted) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0)",
        params![
            &id,
            assistant,
            &ex.session_id,
            &ex.thread_id,
            &ex.source_key,
            ex.ts_ms,
            &ex.cwd,
            repo,
            &ex.git_branch,
            &ex.model,
            &ex.prompt,
            &ex.response,
            commands_text(ex)
        ],
    )?;
    write_derived(tx, &id, ex)?;
    Ok(Written::Inserted)
}

/// The mined command lines, denormalised onto the row so the FTS5 index can be
/// external-content over `exchanges`. The `commands` table stays authoritative;
/// this is a projection of it maintained in the same transaction.
fn commands_text(ex: &ParsedExchange) -> String {
    ex.commands
        .iter()
        .map(|c| c.cmd.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_derived(tx: &rusqlite::Transaction, id: &str, ex: &ParsedExchange) -> Result<()> {
    for (i, c) in ex.commands.iter().enumerate() {
        tx.execute(
            "INSERT INTO commands (exchange_id, seq, cmd, lang) VALUES (?1,?2,?3,?4)",
            params![id, i as i64, &c.cmd, &c.lang],
        )?;
    }
    for (i, f) in ex.files.iter().enumerate() {
        tx.execute(
            "INSERT INTO file_refs (exchange_id, seq, path, tool) VALUES (?1,?2,?3,?4)",
            params![id, i as i64, &f.path, &f.tool],
        )?;
    }
    Ok(())
}

/// ULID randomness without a `rand` dependency. Uniqueness only has to hold
/// within one millisecond on one machine.
fn rand_u128() -> u128 {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut h = RandomState::new().build_hasher();
    h.write_u64(std::process::id() as u64);
    h.write_i64(now_ms());
    h.write_usize(&h as *const _ as usize);
    let a = h.finish() as u128;
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u128(a);
    ((a << 64) | h2.finish() as u128) & ((1u128 << 80) - 1)
}

/// Resolved at write time, never derived from `cwd` at query time — the
/// checkout may be renamed or gone by the time anyone searches.
pub fn resolve_repo(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return d.file_name().map(|n| n.to_string_lossy().into_owned());
        }
        dir = d.parent();
    }
    None
}

pub fn is_ignored(cwd: &Path, ignores: &[PathBuf]) -> bool {
    ignores.iter().any(|i| cwd == i || cwd.starts_with(i))
}

/// Every Claude Code transcript on disk, newest last.
pub fn claude_transcripts(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for project in std::fs::read_dir(root)? {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(project.path())? {
            let f = f?;
            let p = f.path();
            if p.extension().is_some_and(|e| e == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}
