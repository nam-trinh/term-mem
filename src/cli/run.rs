//! `tmem run <repl> [args…]` — capture from a REPL that writes nothing to disk.
//!
//! The last tier and the least good one. See `src/capture/pty.rs` for what it
//! can and cannot promise.

use crate::capture::{self, adapters::pty_repl::PtyRepl, pty};
use crate::db;
use crate::output::{EXIT_ERROR, EXIT_OK};
use crate::paths;
use anyhow::Result;

pub fn run(repl: Option<String>, args: &[String]) -> Result<i32> {
    let Some(name) = repl else {
        println!("tmem run — record a REPL that keeps no transcript of its own.\n");
        println!("  Usage:  tmem run <repl> [args…]\n");
        println!("  Known REPLs:");
        for r in pty::REPLS {
            println!("    {:<10}  {}", r.name, r.about);
        }
        println!(
            "\n  Anything not on that list is refused. term-mem never watches your\n  \
             terminal — capture happens only from programs with an adapter.\n"
        );
        println!("  This tier is lossy: a REPL redraws, and what is recorded is a render");
        println!("  of the screen rather than the model's output. Prefer an assistant that");
        println!("  writes a transcript — `tmem doctor` lists the ones found on this machine.");
        return Ok(EXIT_OK);
    };

    let Some(repl) = pty::find_repl(&name) else {
        anyhow::bail!(
            "`tmem run {name}` is not supported; known REPLs are {}.\n  \
             term-mem never watches the terminal — a program needs an explicit adapter, \
             and adding one is a code change rather than a flag.",
            pty::repl_names()
        );
    };

    if !crate::cli::pause::capture_enabled() {
        eprintln!(
            "tmem: capture is paused — running `{}` without recording",
            repl.name
        );
        // Still run it. Refusing to start the user's program because our
        // recorder is off would be the tool getting in the way, which is the
        // one thing a capture layer must never do.
    }

    let path = pty::run(repl, args)?;

    // The recording is a transcript, and it goes in through the ordinary door:
    // redaction, the tombstone and idempotency are all properties of that path.
    let mut conn = db::open(&paths::db_path()?)?;
    let ignores = crate::cli::ignore::load()?;
    let redactor = crate::redact::Redactor::load()?;
    match capture::ingest_file(&mut conn, &PtyRepl, &path, &ignores, true, &redactor) {
        Ok(s) => {
            if s.inserted > 0 {
                println!("tmem: {} exchange(s) recorded", s.inserted);
            }
            if s.redacted_exchanges > 0 {
                println!(
                    "  redacted: {} exchange(s) — {}",
                    s.redacted_exchanges,
                    s.redactions.summary()
                );
            }
            Ok(EXIT_OK)
        }
        Err(e) => {
            // The recording is still on disk, so say where: losing a captured
            // exchange is the one unacceptable failure.
            eprintln!(
                "tmem: recorded to {} but could not ingest it: {e:#}",
                path.display()
            );
            Ok(EXIT_ERROR)
        }
    }
}
