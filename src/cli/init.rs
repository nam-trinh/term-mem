//! `tmem init` — create the database, wire up capture, and say what is about to
//! be recorded. docs/cli.md: "A tool that silently begins archiving everything
//! you type is one people uninstall in anger."

use crate::capture::{self, adapters};
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
        let files = adapters::discover_all()?;
        let ignores = crate::cli::ignore::load()?;
        let redactor = crate::redact::Redactor::load()?;
        let mut total = 0usize;
        let mut failed = 0usize;
        for (adapter, f) in &files {
            // Log and continue, as `capture` does: one unreadable transcript
            // must not cost the user the other months of history.
            match capture::ingest_file(&mut conn, *adapter, f, &ignores, true, &redactor) {
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

/// Does one hook entry — `{"type": "command", "command": "…"}` — name the
/// command we are looking for?
///
/// The rule is exact equality after normalising away the path the binary was
/// invoked by, so `/usr/local/bin/tmem recall --hook` and `tmem recall --hook`
/// are the same hook. That case is not hypothetical: a user whose `tmem` is not
/// on the hook's `PATH` writes the absolute form by hand, and it is the form
/// `doctor` tells them to write.
///
/// **All three of `add_hook`, `remove_hook` and `hook_registered` go through
/// this.** They used not to — `add` and `registered` matched a substring of the
/// serialised group while `remove` compared the `command` field for equality —
/// and the absolute-path spelling landed in the gap: `status` reported the hook
/// ON, `--disable` reported it OFF, and neither was doing anything. `doctor`
/// then pointed at the command that had just failed silently, forever.
fn entry_names(entry: &Value, command: &str) -> bool {
    let Some(found) = entry.get("command").and_then(Value::as_str) else {
        return false;
    };
    normalise_command(found) == normalise_command(command)
}

/// `"/usr/local/bin/tmem recall --hook"` → `"tmem recall --hook"`. Only the
/// program word is touched; the arguments have to match exactly, because
/// `--hook` and `--drain` are different hooks.
fn normalise_command(command: &str) -> String {
    let command = command.trim();
    let (program, rest) = match command.split_once(char::is_whitespace) {
        Some((p, r)) => (p, r.trim()),
        None => (command, ""),
    };
    let base = program
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(program);
    if rest.is_empty() {
        base.to_string()
    } else {
        format!("{base} {rest}")
    }
}

/// Every hook entry under `hooks.<event>`, flattened across the two shapes
/// settings.json allows: a group with an inner `hooks` array, or a bare entry.
fn entries(list: &[Value]) -> impl Iterator<Item = &Value> {
    list.iter()
        .flat_map(|group| match group.get("hooks").and_then(Value::as_array) {
            Some(inner) => inner.iter().collect::<Vec<_>>(),
            None => vec![group],
        })
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

    if entries(list).any(|e| entry_names(e, command)) {
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
    // Drop the inner entries that name our command, and then the group only if
    // *we* are what emptied it.
    //
    // The earlier version dropped any group whose `hooks` array was empty,
    // which deleted a user's own `{"matcher": "x", "hooks": []}` — a placeholder
    // they had written deliberately — as a side effect of turning recall off.
    // This is not our file. Nothing in it is ours to tidy.
    let mut removed = 0usize;
    list.retain_mut(|group| {
        if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            let before = inner.len();
            inner.retain(|h| !entry_names(h, command));
            let took = before - inner.len();
            removed += took;
            !(took > 0 && inner.is_empty())
        } else if entry_names(group, command) {
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
    // A predicate must not create anything. `status` and `doctor` both call
    // this, and an unconfigured machine used to get a `~/.claude/` directory
    // out of being asked a question.
    if !path.exists() {
        return false;
    }
    let Ok(root) = read_settings(&path) else {
        return false;
    };
    root.get("hooks")
        .and_then(|h| h.get(event))
        .and_then(Value::as_array)
        .map(|l| entries(l).any(|e| entry_names(e, command)))
        .unwrap_or(false)
}

fn read_settings(path: &std::path::Path) -> Result<Value> {
    let display = path.to_string_lossy().into_owned();
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {display}"))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing {display}"))
}

fn write_settings(path: &std::path::Path, root: &Value) -> Result<()> {
    // The directory is created here rather than in `read_settings`, which is
    // also reached by predicates that must not have side effects.
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
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
