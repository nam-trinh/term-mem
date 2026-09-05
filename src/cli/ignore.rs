//! `tmem ignore` — the path-scoped opt-out. docs/cli.md: "the one that sees
//! real use: there's usually one directory whose contents shouldn't be archived
//! even though everything else should."
//!
//! Stored as a plain newline-delimited file, for the same reason pause is:
//! the hook consults it before enqueueing, and it must be greppable and
//! hand-editable like everything else the user owns.

use crate::paths;
use anyhow::Result;
use std::path::PathBuf;

pub fn load() -> Result<Vec<PathBuf>> {
    let p = paths::ignore_file()?;
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Ok(Vec::new());
    };
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .collect())
}

fn save(list: &[PathBuf]) -> Result<()> {
    let p = paths::ignore_file()?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let body: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    std::fs::write(p, body)?;
    Ok(())
}

pub fn run(path: Option<PathBuf>, list: bool, remove: Option<PathBuf>) -> Result<i32> {
    let mut current = load()?;
    if list || (path.is_none() && remove.is_none()) {
        if current.is_empty() {
            println!("no ignored paths");
            return Ok(crate::output::EXIT_EMPTY);
        }
        for p in &current {
            println!("{}", crate::output::tilde(&p.to_string_lossy()));
        }
        return Ok(crate::output::EXIT_OK);
    }
    if let Some(r) = remove {
        let r = std::fs::canonicalize(&r).unwrap_or(r);
        let before = current.len();
        current.retain(|p| p != &r);
        save(&current)?;
        if current.len() == before {
            println!("{} was not ignored", r.display());
            return Ok(crate::output::EXIT_EMPTY);
        }
        println!(
            "no longer ignoring {}",
            crate::output::tilde(&r.to_string_lossy())
        );
        return Ok(crate::output::EXIT_OK);
    }
    if let Some(p) = path {
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        if !current.contains(&p) {
            current.push(p.clone());
            save(&current)?;
        }
        println!("ignoring {}", crate::output::tilde(&p.to_string_lossy()));
        println!("nothing under that tree will be recorded from now on.");
        println!("already-recorded exchanges are untouched. `tmem forget <id>` removes");
        println!("them one at a time; `tmem forget --in <path>` arrives in Phase 2.");
    }
    Ok(crate::output::EXIT_OK)
}
