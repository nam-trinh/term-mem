//! Where term-mem keeps its files. Everything is under the user's own home;
//! nothing here reaches the network.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// `$TMEM_HOME` overrides everything. Tests rely on it, and so does anyone who
/// wants a second archive.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set; cannot locate the term-mem data directory"))
}

pub fn data_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("TMEM_HOME") {
        return Ok(PathBuf::from(p));
    }
    Ok(home()?.join(".local/share/term-mem"))
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("memory.db"))
}

/// Hook writes land here and a drainer picks them up, so the hook itself never
/// pays for a parse. See docs/tech-stack.md, "Tier 2 — Hooks as a trigger".
pub fn queue_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("queue"))
}

pub fn drain_lock() -> Result<PathBuf> {
    Ok(data_dir()?.join("drain.lock"))
}

/// Pause is a file, not a settings row: the hook has to answer "am I paused?"
/// in microseconds, and opening SQLite to find out would blow the 5ms budget.
pub fn pause_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("paused"))
}

/// Likewise for the ignore list, which the hook consults before enqueueing.
pub fn ignore_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("ignore"))
}

/// Claude Code's transcript root.
pub fn claude_projects_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("TMEM_CLAUDE_PROJECTS") {
        return Ok(PathBuf::from(p));
    }
    Ok(home()?.join(".claude/projects"))
}

pub fn claude_settings_file() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("TMEM_CLAUDE_SETTINGS") {
        return Ok(PathBuf::from(p));
    }
    Ok(home()?.join(".claude/settings.json"))
}
