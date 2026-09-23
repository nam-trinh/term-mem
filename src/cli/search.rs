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

    // Never let a term vanish silently. A term matching nothing does not narrow
    // an OR query, it just disappears — so the results are an honest answer to
    // a question the user did not ask. Reported on stderr so it reaches a
    // person without corrupting `--json` on stdout.
    let dead = search::dead_terms(&conn, terms).unwrap_or_default();
    if !dead.is_empty() {
        let quoted = |v: &[String]| {
            v.iter()
                .map(|t| format!("'{t}'"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if dead.len() == terms.len() {
            eprintln!("tmem: no term in that query appears anywhere in the archive");
        } else {
            let live: Vec<String> = terms
                .iter()
                .filter(|t| !dead.contains(t))
                .cloned()
                .collect();
            eprintln!(
                "tmem: {} matched nothing; these results are for {}",
                quoted(&dead),
                quoted(&live)
            );
        }
        // A concrete suggestion where there is one. `phase5` is one token to
        // the tokenizer and two words in the archive, and the fix is a space —
        // which is much easier to act on than being told to quote something.
        for t in &dead {
            if let Some(split) = search::split_alnum_boundary(t) {
                eprintln!("      try: tmem \"{split}\"   (\"{t}\" is one word to the index)");
                break;
            }
        }
    }

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
