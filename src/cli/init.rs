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
        let redactor = crate::redact::Redactor::load()?;
        let mut total = 0usize;
        let mut failed = 0usize;
        for f in &files {
            // Log and continue, as `capture` does: one unreadable transcript
            // must not cost the user the other months of history.
            match capture::ingest_file(&mut conn, &adapter, f, &ignores, true, &redactor) {
                Ok(s) => total += s.inserted,
                Err(e) => {
                    eprintln!("tmem: {}: {e:#}", f.display());
                    failed += 1;
                }
            }
        }
        println!(
            "  backfill    {} exchanges from {} transcripts",
            total,
            files.len() - failed
        );
        if failed > 0 {
            println!("  skipped     {failed} transcript(s) that could not be read (see above)");
        }
    } else {
        println!();
        println!("  Run `tmem init --backfill` to import the transcripts already on disk.");
    }

    println!();
    println!("  Next:       tmem status · tmem doctor · tmem recent");
    Ok(crate::output::EXIT_OK)
}

pub const HOOK_COMMAND: &str = "tmem capture --hook claude-code";

pub enum HookState {
    Added(String),
    AlreadyPresent(String),
}

/// Register the `Stop` hook by editing Claude Code's settings.json in place,
/// preserving everything else in the file.
fn register_hook() -> Result<HookState> {
    add_hook("Stop", HOOK_COMMAND)
}

/// Add one command hook under `hooks.<event>`, preserving everything else in
/// settings.json. Idempotent: an entry already naming the command is left
/// alone.
///
/// Phase 4 made this shared. `tmem recall --enable` registers a
/// `UserPromptSubmit` hook through the same code, because two hand-rolled
/// settings.json editors is two chances to corrupt a file that is not ours.
pub fn add_hook(event: &str, command: &str) -> Result<HookState> {
    let path = paths::claude_settings_file()?;
    let display = path.to_string_lossy().into_owned();
    let mut root = read_settings(&path)?;

    let hooks = root
        .as_object_mut()
        .context("settings.json is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let list = hooks
        .as_object_mut()
        .context("settings.json `hooks` is not a JSON object")?
        .entry(event)
        .or_insert_with(|| json!([]));
    let list = list
        .as_array_mut()
        .with_context(|| format!("settings.json `hooks.{event}` is not an array"))?;

    if serde_json::to_string(&list)?.contains(command) {
        return Ok(HookState::AlreadyPresent(display));
    }
    list.push(json!({ "hooks": [{ "type": "command", "command": command }] }));
    write_settings(&path, &root)?;
    Ok(HookState::Added(display))
}

/// Remove every hook entry naming `command` from `hooks.<event>`, and nothing
/// else. Returns how many were taken out.
pub fn remove_hook(event: &str, command: &str) -> Result<usize> {
    let path = paths::claude_settings_file()?;
    if !path.exists() {
        return Ok(0);
    }
    let mut root = read_settings(&path)?;
    let Some(list) = root
        .get_mut("hooks")
        .and_then(|h| h.get_mut(event))
        .and_then(Value::as_array_mut)
    else {
        return Ok(0);
    };
    // Drop only the *inner* entries that name our command, then drop a group
    // that is left empty. A user's own hooks may share the group and must
    // survive — which is also why the count is of entries removed, not of
    // groups: removing ours from a group that keeps others still changed the
    // file, and a `removed == 0` there would skip the write.
    let mut removed = 0usize;
    list.retain_mut(|group| {
        if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            let before = inner.len();
            inner.retain(|h| h.get("command").and_then(Value::as_str) != Some(command));
            removed += before - inner.len();
            !inner.is_empty()
        } else if group.get("command").and_then(Value::as_str) == Some(command) {
            removed += 1;
            false
        } else {
            true
        }
    });
    if removed > 0 {
        write_settings(&path, &root)?;
    }
    Ok(removed)
}

pub fn hook_registered(event: &str, command: &str) -> bool {
    let Ok(path) = paths::claude_settings_file() else {
        return false;
    };
    let Ok(root) = read_settings(&path) else {
        return false;
    };
    root.get("hooks")
        .and_then(|h| h.get(event))
        .map(|l| {
            serde_json::to_string(l)
                .unwrap_or_default()
                .contains(command)
        })
        .unwrap_or(false)
}

fn read_settings(path: &std::path::Path) -> Result<Value> {
    let display = path.to_string_lossy().into_owned();
    if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {display}"))?;
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        return serde_json::from_str(&text).with_context(|| format!("parsing {display}"));
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    Ok(json!({}))
}

fn write_settings(path: &std::path::Path, root: &Value) -> Result<()> {
    // Write-then-rename: this is the user's file and a half-written
    // settings.json costs them their whole hook configuration, not just ours.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(root)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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
