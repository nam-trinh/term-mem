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
    // docs/plan.md: the redaction count is visible here, because silent
    // redaction leaves the user unable to tell a mangled response from a bad one.
    let rules_path = crate::redact::user_rules_path()?;
    println!(
        "  redacted    {redacted} exchange(s){}",
        if rules_path.exists() {
            format!("   (rules: {})", tilde(&rules_path.to_string_lossy()))
        } else {
            String::new()
        }
    );
    let forgotten = queries::forgotten_count(&conn)?;
    if forgotten > 0 {
        println!("  forgotten   {forgotten}   (kept as keys only, so re-ingest cannot undo it)");
    }
    println!("  encrypted   {}", crate::db::encryption_status(&db_path));

    // Phase 4. What an agent can reach, and whether anything is being injected
    // into prompts — the second of which the user must never have to guess at.
    // `load_or_default`, not `load`. `status` answers "what is in here and is
    // it running?", and a settings file it cannot parse is one line of that
    // answer — not a reason to abandon the other ten, which used to include
    // whether capture was paused.
    let (recall, recall_broken) = crate::cli::recall::Config::load_or_default();
    let recall_hooked = crate::cli::init::hook_registered(
        crate::cli::recall::HOOK_EVENT,
        crate::cli::recall::HOOK_COMMAND,
    );
    println!(
        "  recall      {}",
        match (recall_broken.is_some(), recall.enabled, recall_hooked) {
            (true, _, _) => format!(
                "UNREADABLE SETTINGS, so nothing is injected — {}",
                recall_broken.as_deref().unwrap_or("")
            ),
            _ => match (recall.enabled, recall_hooked) {
                (true, true) => format!(
                    "ON — up to {} exchange(s), ~{} tokens, prepended to prompts",
                    recall.max_exchanges, recall.max_tokens
                ),
                (false, false) => "off (the default) — `tmem recall --enable`".to_string(),
                (c, h) => format!(
                    "INCONSISTENT — config says enabled={c}, {} hook {} — run `tmem recall \
                     --enable` or `--disable`",
                    crate::cli::recall::HOOK_EVENT,
                    if h { "is registered" } else { "is absent" }
                ),
            },
        }
    );

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
    if let Some(where_) = mcp_registered_in() {
        println!("  mcp         registered in {where_} (read-only)");
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

/// Where, if anywhere, `tmem mcp` is registered as an MCP server.
///
/// Three places, because `claude mcp add` writes to a different one per scope,
/// and none of them is `settings.json` — which is where this used to look, so
/// the status line could never appear however the user had registered it.
fn mcp_registered_in() -> Option<String> {
    let names = |v: &serde_json::Value| {
        v.get("mcpServers")
            .and_then(|m| m.as_object())
            .map(|m| {
                m.values()
                    .any(|s| s.get("command").and_then(|c| c.as_str()) == Some("tmem"))
            })
            .unwrap_or(false)
    };
    let read = |p: std::path::PathBuf| -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    };

    // Project scope: .mcp.json beside the checkout the user is standing in.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(v) = read(cwd.join(".mcp.json")) {
            if names(&v) {
                return Some("./.mcp.json".to_string());
            }
        }
    }
    // User and local scope, both inside ~/.claude.json.
    let path = paths::claude_config_file().ok()?;
    let v = read(path.clone())?;
    if names(&v) {
        return Some(tilde(&path.to_string_lossy()));
    }
    let here = std::env::current_dir().ok()?;
    let local = v.get("projects")?.get(here.to_string_lossy().as_ref())?;
    names(local).then(|| format!("{} (this project)", tilde(&path.to_string_lossy())))
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

    // A rule file that does not compile aborts ingest — correctly, because
    // capturing unredacted would be worse. But the drainer is detached with its
    // stderr discarded, so the user sees capture stop and nothing say why.
    // `doctor` is the command whose whole job is answering that.
    let rules_path = crate::redact::user_rules_path()?;
    match crate::redact::Redactor::load() {
        Ok(_) if rules_path.exists() => ok(&format!(
            "redaction rules load from {}",
            tilde(&rules_path.to_string_lossy())
        )),
        Ok(_) => ok("redaction rules load (no user rule file)"),
        Err(e) => {
            problems += bad(&format!(
                "redaction rules will not load, so capture cannot run: {e:#}"
            ));
        }
    }

    // Phase 4. The recall config and its hook are two files that can disagree,
    // and the direction of the disagreement decides whether the user is being
    // injected into without knowing, or believes they are and is not.
    let recall = match crate::cli::recall::Config::load() {
        Ok(c) => Some(c),
        Err(e) => {
            problems += bad(&format!(
                "automatic recall settings will not parse: {e:#} — the hook injects nothing \
                 until this is fixed"
            ));
            None
        }
    };
    if let Some(recall) = recall {
        let hooked = crate::cli::init::hook_registered(
            crate::cli::recall::HOOK_EVENT,
            crate::cli::recall::HOOK_COMMAND,
        );
        match (recall.enabled, hooked) {
            (false, false) => ok("automatic recall is off (the default); nothing is injected"),
            (true, true) => ok(&format!(
                "automatic recall is on: up to {} exchange(s), ~{} tokens, {}",
                recall.max_exchanges,
                recall.max_tokens,
                crate::cli::recall::describe_floor(&recall)
            )),
            (true, false) => {
                problems += bad(
                    "automatic recall says enabled, but no UserPromptSubmit hook is registered — \
                 nothing is being injected (`tmem recall --enable`)",
                )
            }
            (false, true) => {
                problems += bad(
                    "a UserPromptSubmit hook for tmem is registered while recall says disabled — \
                 it injects nothing, but remove it with `tmem recall --disable`",
                )
            }
        }
    }

    // Per adapter, because "no transcripts" means something different for each:
    // a machine with Claude Code and no Codex is normal, and saying so as a
    // problem would train the user to ignore this whole report.
    let mut any = 0usize;
    let mut discovered: Vec<(&'static str, std::path::PathBuf)> = Vec::new();
    for adapter in crate::capture::adapters::all() {
        let root = adapter.transcript_root()?;
        // One walk, not two. `discover` is a recursive read_dir over a whole
        // archive; doing it once for the count and again for the list doubled
        // the cost of `doctor` for no reason.
        let files = if root.exists() {
            adapter.discover(&root).unwrap_or_default()
        } else {
            Vec::new()
        };
        let found = files.len();
        any += found;
        for f in files {
            discovered.push((adapter.name(), f));
        }
        if found > 0 {
            ok(&format!(
                "{found} {} transcript(s) under {}",
                adapter.name(),
                tilde(&root.to_string_lossy())
            ));
        } else {
            note(&format!(
                "no {} transcripts under {} — nothing to capture from it",
                adapter.name(),
                tilde(&root.to_string_lossy())
            ));
        }
    }
    if any == 0 {
        problems += bad("no transcripts found for any supported assistant");
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
        problems += report_coverage(&conn, &discovered)?;
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
fn report_coverage(
    conn: &Connection,
    transcripts: &[(&'static str, std::path::PathBuf)],
) -> Result<usize> {
    let mut problems = 0;
    let mut unseen = 0;
    let mut stale = 0;
    for (assistant, t) in transcripts {
        let p = t.to_string_lossy().into_owned();
        // Keyed by assistant as well as path. Hardcoding `claude-code` here was
        // harmless with one adapter and would have reported every Codex
        // transcript as never ingested with two.
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT bytes, mtime_ms FROM watermarks WHERE assistant = ?1 AND source_path = ?2",
                rusqlite::params![assistant, &p],
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

/// Neither healthy nor broken. A machine with one assistant installed and not
/// another is the ordinary case, and reporting it as a problem is how a health
/// check teaches people to stop reading it.
fn note(msg: &str) {
    println!("  --  {msg}");
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
