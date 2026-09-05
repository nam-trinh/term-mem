//! `tmem capture` — the ingest entrypoint, with three triggers.
//!
//! * `--hook <assistant>`  the `Stop` hook. Budgeted at under 5ms, so it does
//!   nothing but read stdin, drop a file in the queue and spawn a drainer.
//! * `--drain`             the background writer: does the parse and the write.
//! * `--path <file>`       synchronous ingest of one transcript, and
//!   `--all` for every transcript on disk. Tests and `init --backfill` use it.

use crate::capture::{self, adapters::claude_code::ClaudeCode, queue};
use crate::cli::pause;
use crate::db;
use crate::output::{EXIT_ERROR, EXIT_OK};
use crate::paths;
use anyhow::{Context, Result};
use std::io::Read;
use std::path::PathBuf;

pub fn run(
    hook: Option<String>,
    drain: bool,
    path: Option<PathBuf>,
    all: bool,
    quiet: bool,
) -> Result<i32> {
    if let Some(assistant) = hook {
        return run_hook(&assistant);
    }
    if drain {
        return run_drain(quiet);
    }
    if let Some(p) = path {
        return run_paths(&[p], true, quiet);
    }
    if all {
        // --all sweeps the archive and honours the watermark, so a rerun over
        // months of transcripts costs one stat per unchanged file.
        let files = capture::claude_transcripts(&paths::claude_projects_dir()?)?;
        return run_paths(&files, false, quiet);
    }
    anyhow::bail!("tmem capture: give --hook <assistant>, --drain, --path <file>, or --all")
}

/// The hot path. Everything expensive is deliberately downstream of this.
fn run_hook(assistant: &str) -> Result<i32> {
    if assistant != "claude-code" {
        anyhow::bail!(
            "unknown assistant '{assistant}'; Phase 1 ships the claude-code adapter only"
        );
    }
    // TMEM=0 and pause are checked before anything is written, and both are
    // file/env lookups rather than a database open.
    if !pause::capture_enabled() {
        return Ok(EXIT_OK);
    }

    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading hook payload from stdin")?;
    let payload: queue::HookPayload =
        serde_json::from_str(&buf).context("parsing the Stop hook payload")?;
    let Some(transcript) = payload.transcript_path else {
        // Loud on stderr, but exit 0: a hook that fails a turn is worse than a
        // hook that misses one, and the watcher/backfill path still catches it.
        eprintln!("tmem: Stop payload carried no transcript_path; nothing captured");
        return Ok(EXIT_OK);
    };

    if let Some(cwd) = &payload.cwd {
        if capture::is_ignored(std::path::Path::new(cwd), &crate::cli::ignore::load()?) {
            return Ok(EXIT_OK);
        }
    }

    queue::enqueue(
        &paths::queue_dir()?,
        &queue::QueuedCapture {
            assistant: assistant.to_string(),
            transcript_path: transcript,
            session_id: payload.session_id,
            cwd: payload.cwd,
            queued_at_ms: capture::now_ms(),
        },
    )?;

    // Detached: the assistant's turn must not wait on the parse.
    if std::env::var("TMEM_NO_SPAWN").is_err() {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tmem"));
        let _ = std::process::Command::new(exe)
            .args(["capture", "--drain", "--quiet"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    Ok(EXIT_OK)
}

fn run_drain(quiet: bool) -> Result<i32> {
    let lock = queue::DrainLock::acquire(&paths::drain_lock()?)?;
    let Some(_lock) = lock else {
        return Ok(EXIT_OK); // another drainer already has it
    };
    let dir = paths::queue_dir()?;
    let items = queue::drain(&dir)?;
    if items.is_empty() {
        if !quiet {
            println!("queue empty");
        }
        return Ok(EXIT_OK);
    }
    // Collapse repeats: several turns in one session queue the same file.
    let mut paths_seen: Vec<PathBuf> = Vec::new();
    for (_, item) in &items {
        let p = PathBuf::from(&item.transcript_path);
        if !paths_seen.contains(&p) {
            paths_seen.push(p);
        }
    }
    let code = run_paths(&paths_seen, true, quiet)?;
    for (entry, _) in &items {
        let _ = std::fs::remove_file(entry);
    }
    Ok(code)
}

fn run_paths(files: &[PathBuf], force: bool, quiet: bool) -> Result<i32> {
    let mut conn = db::open(&paths::db_path()?)?;
    let ignores = crate::cli::ignore::load()?;
    let adapter = ClaudeCode;
    let mut total = capture::IngestStats::default();
    let mut failed = 0;

    for f in files {
        match capture::ingest_file(&mut conn, &adapter, f, &ignores, force) {
            Ok(s) => {
                total.files_seen += s.files_seen;
                total.files_parsed += s.files_parsed;
                total.files_skipped_unchanged += s.files_skipped_unchanged;
                total.files_ignored += s.files_ignored;
                total.inserted += s.inserted;
                total.updated += s.updated;
                total.report.records_total += s.report.records_total;
                total.report.records_unparsable += s.report.records_unparsable;
                total.report.records_unknown_type += s.report.records_unknown_type;
                total.report.prompts_found += s.report.prompts_found;
                total.report.prompts_without_response += s.report.prompts_without_response;
                total.report.api_errors_skipped += s.report.api_errors_skipped;
                total.report.sidechain_records += s.report.sidechain_records;
                total.report.orphaned_records += s.report.orphaned_records;
                total.report.orphaned_chars += s.report.orphaned_chars;
                for t in s.report.unknown_types {
                    if !total.report.unknown_types.contains(&t) {
                        total.report.unknown_types.push(t);
                    }
                }
            }
            Err(e) => {
                // Loud, and keep going: one broken transcript must not stop the
                // rest of an archive from being captured.
                eprintln!("tmem: {}: {e:#}", f.display());
                failed += 1;
            }
        }
    }

    if !quiet {
        println!(
            "{} new, {} updated, from {} transcript(s) ({} unchanged)",
            total.inserted, total.updated, total.files_parsed, total.files_skipped_unchanged
        );
        let r = &total.report;
        if r.records_unparsable > 0
            || r.records_unknown_type > 0
            || r.prompts_without_response > 0
            || r.api_errors_skipped > 0
        {
            println!(
                "  skipped: {} unparsable, {} unknown-type, {} prompt(s) with no response, {} API error(s)",
                r.records_unparsable, r.records_unknown_type, r.prompts_without_response, r.api_errors_skipped
            );
        }
        if r.orphaned_records > 0 {
            println!(
                "  note: {} assistant record(s) ({} chars) had no prompt in the transcript \
                 and were dropped rather than misattributed",
                r.orphaned_records, r.orphaned_chars
            );
        }
        if !r.unknown_types.is_empty() {
            println!(
                "  note: unrecognised record type(s) {} — the transcript format may have moved",
                r.unknown_types.join(", ")
            );
        }
    }
    Ok(if failed > 0 { EXIT_ERROR } else { EXIT_OK })
}
