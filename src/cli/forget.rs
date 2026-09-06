//! `tmem forget` — the safety valve.
//!
//! Phase 1 ships `--last` and `<id>`. docs/plan.md is explicit that this is not
//! a later feature: "From the first commit that writes to disk, there must be a
//! way to unwrite."
//!
//! Phase 2 adds `--since` and `--in`, the blunt forms docs/scenarios.md argues
//! for: "Blunt is the right default for a safety valve. Precision is a
//! nice-to-have; certainty is the requirement."
//!
//! The FTS index needs no code here. It is maintained by triggers on
//! `exchanges` (migration V3), so a delete that reaches the row reaches the
//! index — which is one fewer table for a future write path to forget.

use crate::db::queries::Filter;
use crate::db::{self, queries};
use crate::output::{EXIT_EMPTY, EXIT_OK};
use crate::paths;
use anyhow::Result;
use std::path::PathBuf;

pub fn run(
    id: Option<String>,
    last: bool,
    since: Option<String>,
    in_path: Option<PathBuf>,
    yes: bool,
) -> Result<i32> {
    let mut conn = db::open(&paths::db_path()?)?;

    // A bulk selector and a single id are different commands wearing one name,
    // and combining them silently would delete more than was asked for.
    let bulk = since.is_some() || in_path.is_some();
    if bulk && (last || id.is_some()) {
        anyhow::bail!(
            "`--since`/`--in` select a set; use them on their own, not with an id or `--last`"
        );
    }

    if bulk {
        let filter = Filter {
            in_paths: in_path
                .as_deref()
                .map(crate::cli::in_path_candidates)
                .unwrap_or_default(),
            since_ms: since
                .as_deref()
                .map(crate::cli::timespec::parse)
                .transpose()?,
            repo: None,
            limit: None,
        };
        return forget_many(&mut conn, &filter, yes);
    }

    let target = if last {
        queries::last_id(&conn)?
    } else if let Some(id) = id {
        queries::resolve_id(&conn, &id)?
    } else {
        anyhow::bail!("forget what? give an id, `--last`, `--since <when>` or `--in <path>`");
    };

    let Some(target) = target else {
        eprintln!("tmem: nothing to forget");
        return Ok(EXIT_EMPTY);
    };

    let Some(ex) = queries::get(&conn, &target)? else {
        eprintln!("tmem: no exchange {target}");
        return Ok(EXIT_EMPTY);
    };

    // Gated on *stdin*, not stdout: `tmem forget <id> | tee log` is still a
    // person at a keyboard, and this is the one command that cannot be undone.
    if !yes && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!("about to permanently delete:");
        println!();
        println!(
            "  {}  {}  {}",
            ex.id,
            crate::output::fmt_date(ex.ts),
            crate::output::tilde(&ex.cwd)
        );
        println!("  \"{}\"", first_line(&ex.prompt, 88));
        println!();
        print!("delete it? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("nothing deleted");
            return Ok(EXIT_EMPTY);
        }
    }

    let n = queries::forget(&mut conn, std::slice::from_ref(&target))?;
    println!("forgot {n} exchange{}", if n == 1 { "" } else { "s" });
    println!("the row, its commands, its file references and its index entries are gone, and the database file has been vacuumed.");
    Ok(EXIT_OK)
}

/// The blunt form. Shows what is about to go — every row, not a count, because
/// a number is not something a person can check before an irreversible command.
fn forget_many(conn: &mut rusqlite::Connection, filter: &Filter, yes: bool) -> Result<i32> {
    let rows = queries::list(conn, filter)?;
    if rows.is_empty() {
        eprintln!("tmem: nothing matches — nothing deleted");
        return Ok(EXIT_EMPTY);
    }
    if !yes && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "about to permanently delete {} exchange{}:",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        );
        println!();
        for ex in rows.iter().take(20) {
            println!(
                "  {}  {}  {}",
                ex.id,
                crate::output::fmt_date(ex.ts),
                crate::output::tilde(&ex.cwd)
            );
            println!("  \"{}\"", first_line(&ex.prompt, 88));
        }
        if rows.len() > 20 {
            println!("  … and {} more", rows.len() - 20);
        }
        println!();
        print!(
            "delete {}? [y/N] ",
            if rows.len() == 1 { "it" } else { "them all" }
        );
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("nothing deleted");
            return Ok(EXIT_EMPTY);
        }
    }
    let ids: Vec<String> = rows.iter().map(|e| e.id.clone()).collect();
    let n = queries::forget(conn, &ids)?;
    println!("forgot {n} exchange{}", if n == 1 { "" } else { "s" });
    println!("the rows, their commands, their file references and their index entries are gone, and the database file has been vacuumed.");
    Ok(EXIT_OK)
}

fn first_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}
