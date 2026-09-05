//! The hook's hot path.
//!
//! `docs/plan.md` budgets the `Stop` hook at **under 5ms**, and
//! `docs/tech-stack.md` spells out how: "serialize, append to a queue, return".
//! So the hook does not open SQLite and does not parse anything. It writes one
//! small file and spawns a detached drainer.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueuedCapture {
    pub assistant: String,
    pub transcript_path: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub queued_at_ms: i64,
}

/// The `Stop` payload. Only three fields matter, and the response text is not
/// among them — the hook is a trigger, not a source.
#[derive(Debug, Deserialize)]
pub struct HookPayload {
    pub transcript_path: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

pub fn enqueue(dir: &Path, item: &QueuedCapture) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = format!("{:013}-{}.json", item.queued_at_ms, std::process::id());
    let path = dir.join(name);
    // Write-then-rename so a drainer never reads a half-written entry.
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(serde_json::to_string(item)?.as_bytes())?;
    f.flush()?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn drain(dir: &Path) -> Result<Vec<(PathBuf, QueuedCapture)>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();
    for p in entries {
        match std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(item) => out.push((p, item)),
            None => {
                // Unreadable entry: drop it rather than wedge the queue, but say so.
                eprintln!("tmem: discarding unreadable queue entry {}", p.display());
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    Ok(out)
}

pub fn len(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
        .unwrap_or(0)
}

/// A crude single-holder lock so a burst of turns spawns one drainer, not ten.
/// `O_EXCL` create; a lock older than five minutes is treated as abandoned.
pub struct DrainLock(PathBuf);

impl DrainLock {
    pub fn acquire(path: &Path) -> Result<Option<DrainLock>> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        if let Ok(meta) = std::fs::metadata(path) {
            let stale = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs() > 300)
                .unwrap_or(true);
            if stale {
                let _ = std::fs::remove_file(path);
            }
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => {
                let _ = write!(f, "{}", std::process::id());
                Ok(Some(DrainLock(path.to_path_buf())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for DrainLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
