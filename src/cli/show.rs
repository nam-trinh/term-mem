//! `tmem recent`, `tmem log`, `tmem show`. Browsing is the whole of Phase 1's
//! retrieval surface: search arrives in Phase 2, and a tool that captures
//! reliably and lets you scroll what it captured is already useful.

use crate::cli::BrowseArgs;
use crate::db::{self, queries};
use crate::output::{self, EXIT_EMPTY, EXIT_OK};
use crate::paths;
use anyhow::Result;

pub fn list(args: &BrowseArgs) -> Result<i32> {
    let conn = db::open(&paths::db_path()?)?;
    let rows = queries::list(&conn, &args.to_filter()?)?;
    if rows.is_empty() {
        if !args.json {
            eprintln!("tmem: nothing recorded yet — `tmem doctor` will say why");
        }
        return Ok(EXIT_EMPTY);
    }
    if args.json {
        output::print_json(&rows)?;
    } else {
        output::print_list(&rows);
    }
    Ok(EXIT_OK)
}

pub fn show(id: &str, session: bool, json: bool) -> Result<i32> {
    let conn = db::open(&paths::db_path()?)?;
    let Some(resolved) = queries::resolve_id(&conn, id)? else {
        eprintln!("tmem: no exchange matching '{id}'");
        return Ok(EXIT_EMPTY);
    };

    // Grouped on thread_id, not session_id: `/clear` starts a fresh tree in the
    // same file under the same session id (docs/phases/phase-0.md finding 5).
    let rows = if session {
        queries::thread(&conn, &resolved)?
    } else {
        queries::get(&conn, &resolved)?.into_iter().collect()
    };

    if rows.is_empty() {
        return Ok(EXIT_EMPTY);
    }
    if json {
        output::print_json(&rows)?;
        return Ok(EXIT_OK);
    }
    for (i, ex) in rows.iter().enumerate() {
        if i > 0 {
            println!("\n{}\n", "─".repeat(72));
        }
        output::print_full(ex);
    }
    Ok(EXIT_OK)
}
