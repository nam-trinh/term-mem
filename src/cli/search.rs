//! `tmem <query>` — the default verb, and `tmem search <query>` for scripts and
//! for queries that collide with a subcommand name.

use crate::cli::BrowseArgs;
use crate::db;
use crate::output::{self, EXIT_EMPTY, EXIT_OK};
use crate::paths;
use crate::search;
use anyhow::Result;

pub fn run(terms: &[String], args: &BrowseArgs) -> Result<i32> {
    let db_path = paths::db_path()?;
    if !db_path.exists() {
        anyhow::bail!("no archive yet — run `tmem init`");
    }
    let conn = db::open(&db_path)?;
    let hits = search::search(&conn, terms, &args.to_filter()?)?;

    if hits.is_empty() {
        if !args.json {
            // Exit 1, and say what to try instead: docs/cli.md is clear that a
            // query with no overlapping terms finds nothing, and that browsing
            // by time and place is the backstop rather than a consolation.
            eprintln!(
                "tmem: nothing matched. Browsing is the backstop:\n  \
                 tmem recent\n  tmem log --in <path> --since <when>"
            );
        }
        return Ok(EXIT_EMPTY);
    }
    if args.json {
        output::print_json(&hits)?;
    } else {
        output::print_hits(&hits);
    }
    Ok(EXIT_OK)
}
