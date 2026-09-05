//! `tmem forget` — the safety valve.
//!
//! Phase 1 ships `--last` and `<id>`. docs/plan.md is explicit that this is not
//! a later feature: "From the first commit that writes to disk, there must be a
//! way to unwrite."
//!
//! `--since` and `--in` belong to Phase 2, where `forget` also becomes
//! responsible for the FTS index and the derived command rows.

use crate::db::{self, queries};
use crate::output::{EXIT_EMPTY, EXIT_OK};
use crate::paths;
use anyhow::Result;

pub fn run(id: Option<String>, last: bool, yes: bool) -> Result<i32> {
    let mut conn = db::open(&paths::db_path()?)?;

    let target = if last {
        queries::last_id(&conn)?
    } else if let Some(id) = id {
        queries::resolve_id(&conn, &id)?
    } else {
        anyhow::bail!("forget what? give an id, or `--last`");
    };

    let Some(target) = target else {
        eprintln!("tmem: nothing to forget");
        return Ok(EXIT_EMPTY);
    };

    let Some(ex) = queries::get(&conn, &target)? else {
        eprintln!("tmem: no exchange {target}");
        return Ok(EXIT_EMPTY);
    };

    if !yes && crate::output::is_tty() {
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
    println!("the row, its commands and its file references are gone, and the database file has been vacuumed.");
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
