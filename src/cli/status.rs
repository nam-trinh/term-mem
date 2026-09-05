//! `tmem status` and `tmem doctor`.
//!
//! `status` answers "what is in here and is it running?"; `doctor` answers "is
//! capture actually wired up?" — and, because silent failure is the enemy,
//! reports what the last parse skipped rather than keeping it to itself.

use crate::cli::pause::{self, Pause};
use crate::db::{self, queries};
use crate::output::{tilde, EXIT_ERROR, EXIT_OK};
use crate::paths;
use anyhow::Result;
use rusqlite::Connection;

pub fn status() -> Result<i32> {
    let db_path = paths::db_path()?;
    if !db_path.exists() {
        println!("no archive yet — run `tmem init`");
        return Ok(EXIT_ERROR);
    }
    let conn = db::open(&db_path)?;
    let n = queries::count(&conn)?;
    let (oldest, newest): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT MIN(ts), MAX(ts) FROM exchanges", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let commands: i64 = conn.query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))?;
    let redacted: i64 = conn.query_row(
        "SELECT COUNT(*) FROM exchanges WHERE redacted = 1",
        [],
        |r| r.get(0),
    )?;
    let sessions: i64 =
        conn.query_row("SELECT COUNT(DISTINCT thread_id) FROM exchanges", [], |r| {
            r.get(0)
        })?;
    let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

    println!("  archive     {}", tilde(&db_path.to_string_lossy()));
    println!("  size        {}", human_bytes(size));
    println!("  exchanges   {n}");
    println!("  threads     {sessions}");
    println!("  commands    {commands}");
    if let (Some(o), Some(nw)) = (oldest, newest) {
        println!(
            "  span        {} … {}",
            crate::output::fmt_date(o),
            crate::output::fmt_date(nw)
        );
    }
    // docs/plan.md Phase 3: the redaction count is visible here, because silent
    // redaction leaves the user unable to tell a mangled response from a bad one.
    println!("  redacted    {redacted}   (redaction lands in Phase 3)");
    let forgotten = queries::forgotten_count(&conn)?;
    if forgotten > 0 {
        println!("  forgotten   {forgotten}   (kept as keys only, so re-ingest cannot undo it)");
    }
    println!("  encrypted   no    (opt-in encryption lands in Phase 3)");

    // Pause state must be visible.
    match pause::state()? {
        Pause::No => println!("  capture     ON"),
        Pause::Indefinite => println!("  capture     PAUSED — `tmem resume` to restart"),
        Pause::Until(t) => println!(
            "  capture     PAUSED until {}",
            crate::output::fmt_datetime(t)
        ),
    }
    if std::env::var("TMEM").map(|v| v == "0").unwrap_or(false) {
        println!("  note        TMEM=0 is set in this shell; capture is off for it");
    }
    let ignored = crate::cli::ignore::load()?;
    if !ignored.is_empty() {
        println!(
            "  ignoring    {} path(s) — `tmem ignore --list`",
            ignored.len()
        );
    }
    let q = crate::capture::queue::len(&paths::queue_dir()?);
    if q > 0 {
        println!("  queued      {q} capture(s) waiting — `tmem capture --drain`");
    }
    Ok(EXIT_OK)
}

pub fn doctor() -> Result<i32> {
    let mut problems = 0;
    println!("checking capture…\n");

    let db_path = paths::db_path()?;
    if db_path.exists() {
        ok(&format!(
            "database present at {}",
            tilde(&db_path.to_string_lossy())
        ));
    } else {
        problems += bad("no database — run `tmem init`");
    }

    let settings = paths::claude_settings_file()?;
    let hook_ok = std::fs::read_to_string(&settings)
        .map(|s| s.contains("tmem capture --hook"))
        .unwrap_or(false);
    if hook_ok {
        ok(&format!(
            "Stop hook registered in {}",
            tilde(&settings.to_string_lossy())
        ));
    } else {
        problems += bad(&format!(
            "no Stop hook in {} — run `tmem init`, or capture only happens on backfill",
            tilde(&settings.to_string_lossy())
        ));
    }

    if !hook_is_on_path() {
        problems += bad("`tmem` is not on PATH — the hook fires but cannot find the binary");
    } else {
        ok("`tmem` resolves on PATH");
    }

    let root = paths::claude_projects_dir()?;
    let transcripts = crate::capture::claude_transcripts(&root).unwrap_or_default();
    if transcripts.is_empty() {
        problems += bad(&format!(
            "no Claude Code transcripts under {}",
            tilde(&root.to_string_lossy())
        ));
    } else {
        ok(&format!(
            "{} Claude Code transcript(s) under {}",
            transcripts.len(),
            tilde(&root.to_string_lossy())
        ));
    }

    match pause::state()? {
        Pause::No => ok("capture is not paused"),
        Pause::Indefinite => {
            problems += bad("capture is PAUSED — nothing is being recorded (`tmem resume`)")
        }
        Pause::Until(t) => {
            problems += bad(&format!(
                "capture is PAUSED until {} (`tmem resume`)",
                crate::output::fmt_datetime(t)
            ))
        }
    }

    if db_path.exists() {
        let conn = db::open(&db_path)?;
        problems += report_coverage(&conn, &transcripts)?;
    }

    println!();
    if problems == 0 {
        println!("capture looks healthy.");
        Ok(EXIT_OK)
    } else {
        println!("{problems} problem(s) above.");
        Ok(EXIT_ERROR)
    }
}

/// What ingest has and has not seen. This is the part that makes a silent
/// parser failure loud: a transcript on disk with no watermark row means the
/// file was never read, and that is invisible from `status` alone.
fn report_coverage(conn: &Connection, transcripts: &[std::path::PathBuf]) -> Result<usize> {
    let mut problems = 0;
    let mut unseen = 0;
    let mut stale = 0;
    for t in transcripts {
        let p = t.to_string_lossy().into_owned();
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT bytes, mtime_ms FROM watermarks WHERE assistant = 'claude-code' AND source_path = ?1",
                rusqlite::params![&p],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match row {
            None => unseen += 1,
            Some((b, _)) => {
                if std::fs::metadata(t).map(|m| m.len() as i64).unwrap_or(b) != b {
                    stale += 1;
                }
            }
        }
    }
    if unseen > 0 {
        problems += bad(&format!(
            "{unseen} transcript(s) never ingested — `tmem init --backfill`"
        ));
    } else if !transcripts.is_empty() {
        ok("every transcript on disk has been ingested at least once");
    }
    if stale > 0 {
        println!("  ·  {stale} transcript(s) have grown since the last ingest (a `tmem capture --drain` away)");
    }
    Ok(problems)
}

fn hook_is_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| d.join("tmem").is_file())
}

fn ok(msg: &str) {
    println!("  ok  {msg}");
}

fn bad(msg: &str) -> usize {
    println!("  !!  {msg}");
    1
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
