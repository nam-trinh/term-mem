//! term-mem — a local memory layer for terminal AI conversations.
//!
//! Phase 1: capture and browse. Search arrives in Phase 2 — see docs/plan.md.
//!
//! There is no network code in this binary, by design and by promise. See
//! docs/mission.md: nothing leaves the machine.

mod capture;
mod cli;
mod db;
mod output;
mod paths;

use clap::{Parser, Subcommand};
use output::{EXIT_ERROR, EXIT_OK};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "tmem",
    about = "Local memory for terminal AI conversations",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Search terms. Search is the default verb — but it lands in Phase 2.
    #[arg(trailing_var_arg = true, hide = true)]
    query: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Create the archive, register capture, and say what gets recorded
    Init {
        /// Also import the transcripts already on disk
        #[arg(long)]
        backfill: bool,
        /// Do not touch Claude Code's settings.json
        #[arg(long)]
        no_hook: bool,
    },
    /// What is in the archive, and is capture running
    Status,
    /// Is capture actually wired up
    Doctor,
    /// Ingest transcripts (used by the Stop hook; also runnable by hand)
    Capture {
        /// Run as a hook for the named assistant, reading the payload on stdin
        #[arg(long, value_name = "ASSISTANT")]
        hook: Option<String>,
        /// Process anything the hook queued
        #[arg(long)]
        drain: bool,
        /// Ingest one transcript file
        #[arg(long, value_name = "FILE")]
        path: Option<PathBuf>,
        /// Ingest every transcript on disk
        #[arg(long)]
        all: bool,
        /// Say nothing on success
        #[arg(long)]
        quiet: bool,
    },
    /// Latest exchanges
    Recent {
        #[command(flatten)]
        browse: cli::BrowseArgs,
    },
    /// Everything from a directory tree, oldest context first
    Log {
        #[command(flatten)]
        browse: cli::BrowseArgs,
    },
    /// One exchange, in full
    Show {
        id: String,
        /// The surrounding conversation thread
        #[arg(long)]
        session: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Stop recording, globally
    Pause {
        /// Auto-resume after this long, e.g. `2h`
        duration: Option<String>,
    },
    /// Start recording again
    Resume,
    /// Never record anything under a path
    Ignore {
        path: Option<PathBuf>,
        /// Show the ignore list
        #[arg(long)]
        list: bool,
        /// Stop ignoring a path
        #[arg(long, value_name = "PATH")]
        remove: Option<PathBuf>,
    },
    /// Permanently delete an exchange
    Forget {
        id: Option<String>,
        /// The most recent exchange
        #[arg(long)]
        last: bool,
        /// Do not ask
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

fn main() {
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tmem: {e:#}");
            EXIT_ERROR
        }
    };
    std::process::exit(code);
}

fn run() -> anyhow::Result<i32> {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        if !cli.query.is_empty() {
            // docs/cli.md makes search the default verb. Phase 2 implements it;
            // until then say so plainly rather than printing an empty result,
            // which would look like the archive had lost the exchange.
            eprintln!(
                "tmem: search lands in Phase 2. For now, browse what was captured:\n  \
                 tmem recent\n  tmem log --in <path>\n  tmem show <id>"
            );
            return Ok(EXIT_ERROR);
        }
        return cli::status::status();
    };

    match command {
        Command::Init { backfill, no_hook } => cli::init::run(backfill, no_hook),
        Command::Status => cli::status::status(),
        Command::Doctor => cli::status::doctor(),
        Command::Capture {
            hook,
            drain,
            path,
            all,
            quiet,
        } => cli::capture_cmd::run(hook, drain, path, all, quiet),
        Command::Recent { browse } => cli::show::list(&browse),
        Command::Log { browse } => cli::show::list(&browse),
        Command::Show { id, session, json } => cli::show::show(&id, session, json),
        Command::Pause { duration } => cli::pause::pause(duration.as_deref()),
        Command::Resume => cli::pause::resume(),
        Command::Ignore { path, list, remove } => cli::ignore::run(path, list, remove),
        Command::Forget { id, last, yes } => cli::forget::run(id, last, yes),
    }
    .map(|c| if c == EXIT_OK { EXIT_OK } else { c })
}
