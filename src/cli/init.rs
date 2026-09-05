//! `tmem init` — create the database, wire up capture, and say what is about to
//! be recorded. docs/cli.md: "A tool that silently begins archiving everything
//! you type is one people uninstall in anger."

use crate::capture::{self, adapters::claude_code::ClaudeCode};
use crate::db;
use crate::paths;
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub fn run(backfill: bool, no_hook: bool) -> Result<i32> {
    let db_path = paths::db_path()?;
    let existed = db_path.exists();
    let mut conn = db::open(&db_path)?;

    println!("term-mem");
    println!();
    println!(
        "  database    {}{}",
        crate::output::tilde(&db_path.to_string_lossy()),
        if existed {
            " (already present)"
        } else {
            " (created)"
        }
    );

    // docs/cli.md: silently shadowing an existing command is hostile.
    if let Some(other) = shadowed_binary() {
        println!();
        println!("  warning     `tmem` already resolves to {other}");
        println!("              term-mem will shadow it once installed earlier on PATH.");
        println!("              Install under another name, or reorder PATH, if that matters.");
    }

    println!();
    println!("  What gets recorded, for every assistant with an adapter:");
    println!("    · the prompt you typed and the response you got back");
    println!("    · working directory, git repo and branch, timestamp, model");
    println!("    · command lines the assistant ran, and the paths it touched");
    println!();
    println!("  What does not:");
    println!("    · anything from a program without an adapter — term-mem never");
    println!("      watches your terminal, only transcripts assistants write");
    println!("    · the assistant's reasoning blocks, and file contents");
    println!("    · nothing at all leaves this machine; there is no network code");
    println!();
    println!("  Turning it off:   tmem pause · tmem ignore <path> · TMEM=0 <assistant>");
    println!("  Undoing it:       tmem forget --last · tmem forget <id>");
    println!();

    if no_hook {
        println!("  hook        skipped (--no-hook)");
    } else {
        match register_hook() {
            Ok(HookState::Added(p)) => {
                println!("  hook        registered in {}", crate::output::tilde(&p))
            }
            Ok(HookState::AlreadyPresent(p)) => {
                println!(
                    "  hook        already registered in {}",
                    crate::output::tilde(&p)
                )
            }
            Err(e) => {
                println!("  hook        NOT registered: {e:#}");
                println!(
                    "              add this to the `Stop` hooks in your Claude Code settings:"
                );
                println!(
                    "                {{\"type\": \"command\", \"command\": \"{HOOK_COMMAND}\"}}"
                );
            }
        }
    }

    if backfill {
        println!();
        println!("  Backfilling existing transcripts…");
        let root = paths::claude_projects_dir()?;
        let files = capture::claude_transcripts(&root)?;
        let ignores = crate::cli::ignore::load()?;
        let adapter = ClaudeCode;
        let mut total = 0usize;
        for f in &files {
            let s = capture::ingest_file(&mut conn, &adapter, f, &ignores, true)?;
            total += s.inserted;
        }
        println!(
            "  backfill    {} exchanges from {} transcripts",
            total,
            files.len()
        );
    } else {
        println!();
        println!("  Run `tmem init --backfill` to import the transcripts already on disk.");
    }

    println!();
    println!("  Next:       tmem status · tmem doctor · tmem recent");
    Ok(crate::output::EXIT_OK)
}

const HOOK_COMMAND: &str = "tmem capture --hook claude-code";

enum HookState {
    Added(String),
    AlreadyPresent(String),
}

/// Register the `Stop` hook by editing Claude Code's settings.json in place,
/// preserving everything else in the file.
fn register_hook() -> Result<HookState> {
    let path = paths::claude_settings_file()?;
    let display = path.to_string_lossy().into_owned();
    let mut root: Value = if path.exists() {
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {display}"))?;
        if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&text).with_context(|| format!("parsing {display}"))?
        }
    } else {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        json!({})
    };

    let hooks = root
        .as_object_mut()
        .context("settings.json is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let stop = hooks
        .as_object_mut()
        .context("settings.json `hooks` is not a JSON object")?
        .entry("Stop")
        .or_insert_with(|| json!([]));
    let stop = stop
        .as_array_mut()
        .context("settings.json `hooks.Stop` is not an array")?;

    if serde_json::to_string(&stop)?.contains(HOOK_COMMAND) {
        return Ok(HookState::AlreadyPresent(display));
    }
    stop.push(json!({ "hooks": [{ "type": "command", "command": HOOK_COMMAND }] }));

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&root)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(HookState::Added(display))
}

/// Is there already a `tmem` on PATH that is not us?
fn shadowed_binary() -> Option<String> {
    let me = std::env::current_exe().ok();
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join("tmem");
        if !cand.is_file() {
            continue;
        }
        let resolved = std::fs::canonicalize(&cand).unwrap_or(cand.clone());
        if me.as_ref().and_then(|m| std::fs::canonicalize(m).ok()) == Some(resolved.clone()) {
            return None; // that's us, first on PATH
        }
        return Some(resolved.to_string_lossy().into_owned());
    }
    None
}
