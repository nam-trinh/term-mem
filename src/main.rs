//! term-mem — a local memory layer for terminal AI conversations.
//!
//! Phase 4: capture, browse, keyword recall, redaction, honest deletion, and
//! reuse — memory going back into a live session. See docs/plan.md.
//!
//! There is no network code in this binary, by design and by promise. See
//! docs/mission.md: nothing leaves the machine.

mod capture;
mod cli;
mod db;
mod mcp;
mod output;
mod paths;
mod redact;
mod search;

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

    /// Search terms. Anything that is not a recognized subcommand is a query.
    ///
    /// Deliberately *not* `trailing_var_arg`: docs/scenarios.md types
    /// `tmem backfill --repo --since january`, with the flags after the query,
    /// and a trailing-var-arg would swallow them into the search terms.
    query: Vec<String>,

    #[command(flatten)]
    browse: cli::BrowseArgs,
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
        /// Ingest every transcript on disk, for every assistant
        #[arg(long)]
        all: bool,
        /// Say nothing on success
        #[arg(long)]
        quiet: bool,
        /// Which adapter owns `--path`, when the filename cannot say
        #[arg(long, value_name = "NAME")]
        assistant: Option<String>,
    },
    /// Search the archive — the explicit form of the default verb
    Search {
        query: Vec<String>,
        #[command(flatten)]
        browse: cli::BrowseArgs,
    },
    /// Latest exchanges
    Recent {
        #[command(flatten)]
        browse: cli::BrowseArgs,
    },
    /// Everything from a directory tree, newest first
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
    /// Write the archive out in an open format
    ///
    /// JSON by default — `--json` is accepted and means the same thing, so the
    /// form in docs/cli.md (`tmem export --json | --markdown`) works as printed.
    Export {
        /// Human-readable markdown instead of JSON
        #[arg(long)]
        markdown: bool,
        #[command(flatten)]
        browse: cli::BrowseArgs,
    },
    /// Read exchanges back in from an export
    Import { path: PathBuf },
    /// Serve the archive to an agent over the Model Context Protocol (stdio)
    ///
    /// Read-only. Register it with:
    ///   claude mcp add term-mem -- tmem mcp
    Mcp,
    /// Print the agent tool definitions, for a model that is not an MCP client
    Tools {
        /// Which envelope: `openai` (function calling) or `mcp`
        #[arg(long, default_value = "openai")]
        schema: String,
    },
    /// Run one agent tool and print its JSON result
    Call {
        /// search_memory, get_exchange, or recent
        tool: String,
        /// Arguments as a JSON object; `-` reads them from stdin
        #[arg(long, value_name = "JSON")]
        args: Option<String>,
    },
    /// Format `--json` records on stdin as a context block to prepend
    Render {
        /// The context block. Currently the only rendering there is.
        #[arg(long = "prompt-block")]
        prompt_block: bool,
        /// At most this many exchanges
        #[arg(long, short = 'n')]
        limit: Option<usize>,
        /// Roughly this many tokens, estimated at four characters each
        #[arg(long, value_name = "N")]
        max_tokens: Option<usize>,
    },
    /// Automatic recall on prompt submit — off by default
    Recall {
        /// Words from a prompt: show what would be injected, and why
        query: Vec<String>,
        /// Turn it on and register the UserPromptSubmit hook
        #[arg(long)]
        enable: bool,
        /// Turn it off and remove the hook
        #[arg(long)]
        disable: bool,
        /// Run as the UserPromptSubmit hook, reading its payload on stdin
        #[arg(long)]
        hook: bool,
    },
    /// Record a REPL that keeps no transcript of its own (lossy; last resort)
    Run {
        /// Which REPL: run `tmem run` with no arguments to list them
        repl: Option<String>,
        /// Arguments passed straight through to it
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Permanently delete an exchange
    Forget {
        id: Option<String>,
        /// The most recent exchange
        #[arg(long)]
        last: bool,
        /// Everything since a point in time, e.g. `1 hour ago`
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
        /// Everything recorded under a directory tree
        #[arg(long = "in", value_name = "PATH")]
        in_path: Option<PathBuf>,
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
        // docs/cli.md: search is the default verb, because it is ~95% of
        // invocations and this tool has a dominant one.
        if !cli.query.is_empty() {
            return cli::search::run(&cli.query, &cli.browse);
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
            assistant,
        } => cli::capture_cmd::run(hook, drain, path, all, quiet, assistant),
        Command::Search { query, browse } => cli::search::run(&query, &browse),
        Command::Export { markdown, browse } => {
            let format = if markdown {
                cli::export::Format::Markdown
            } else {
                cli::export::Format::Json
            };
            cli::export::export(format, &browse)
        }
        Command::Import { path } => cli::export::import(&path),
        Command::Recent { browse } => cli::show::list(&browse),
        Command::Log { browse } => cli::show::list(&browse),
        Command::Show { id, session, json } => cli::show::show(&id, session, json),
        Command::Pause { duration } => cli::pause::pause(duration.as_deref()),
        Command::Resume => cli::pause::resume(),
        Command::Ignore { path, list, remove } => cli::ignore::run(path, list, remove),
        Command::Mcp => cli::mcp_serve(),
        Command::Tools { schema } => cli::tools::tools(&schema),
        Command::Call { tool, args } => cli::tools::call(&tool, args.as_deref()),
        Command::Render {
            prompt_block,
            limit,
            max_tokens,
        } => {
            if !prompt_block {
                anyhow::bail!(
                    "tmem render: say what to render — `--prompt-block` is the only \
                     rendering there is"
                );
            }
            cli::render::run(limit, max_tokens)
        }
        Command::Recall {
            query,
            enable,
            disable,
            hook,
        } => match (enable, disable, hook) {
            (true, true, _) => anyhow::bail!("tmem recall: --enable and --disable disagree"),
            (true, _, _) => cli::recall::enable(),
            (_, true, _) => cli::recall::disable(),
            (_, _, true) => cli::recall::hook(),
            _ if !query.is_empty() => cli::recall::preview(&query),
            _ => cli::recall::status(),
        },
        Command::Run { repl, args } => cli::run::run(repl, &args),
        Command::Forget {
            id,
            last,
            since,
            in_path,
            yes,
        } => cli::forget::run(id, last, since, in_path, yes),
    }
    .map(|c| if c == EXIT_OK { EXIT_OK } else { c })
}
