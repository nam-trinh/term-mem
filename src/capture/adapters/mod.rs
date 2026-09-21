//! The parser interface.
//!
//! Phase 1 has exactly one adapter, but the interface is shaped by
//! `docs/phases/codex-cli-format.md`, which found that two of the obvious
//! assumptions are Claude-Code-specific:
//!
//! * **Dedup keys are not universal.** `(session_id, uuid)` is Claude Code's
//!   key. Codex CLI records carry no identifier at all, so its key must be
//!   positional. Each adapter declares its own.
//! * **Injected-block vocabularies are not universal.** Both vendors inject
//!   non-prompt content into user-role records, but with different tag sets —
//!   and Claude Code's must be *rejected* while Codex's must be *stripped*.
//!
//! Phase 6 added a third, which the survey did not predict because it was
//! looking at file *contents*: **where the transcripts are is not universal
//! either**, and it is not a glob the pipeline can hold. Claude Code writes
//! `<project>/*.jsonl` plus `<project>/<session>/subagents/agent-*.jsonl`;
//! Codex CLI writes `sessions/YYYY/MM/DD/rollout-*.jsonl` and keeps
//! non-transcript JSONL in the same tree — a `**/*.jsonl` sweep of `~/.codex`
//! picks up `session_index.jsonl` and a plugin fixture, neither of which is a
//! conversation. So discovery belongs to the adapter too.
//!
//! So all three are properties of the adapter, not of the ingest pipeline.

pub mod claude_code;
pub mod codex;
pub mod pty_repl;

use std::path::{Path, PathBuf};

/// How an adapter identifies a record for idempotency purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupKey {
    /// A stable per-record identifier the format supplies. Claude Code's `uuid`.
    Intrinsic(String),
    /// Position in an append-only file. The only key Codex CLI can offer; a
    /// genuinely weaker guarantee, and one that holds only while the file is
    /// never rewritten in place.
    Positional { line: usize },
}

impl DedupKey {
    pub fn as_str(&self) -> String {
        match self {
            DedupKey::Intrinsic(s) => s.clone(),
            DedupKey::Positional { line } => format!("@{line}"),
        }
    }
}

/// What one folded prompt/response pair looks like before it reaches the
/// database. Adapters produce these; the ingest pipeline writes them.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedExchange {
    pub session_id: String,
    pub thread_id: String,
    pub source_key: String,
    pub ts_ms: i64,
    pub cwd: String,
    /// The repository, when the adapter is *told* rather than having to guess.
    ///
    /// Claude Code leaves this `None` and the pipeline resolves it by walking
    /// up from `cwd` looking for a `.git`. Codex CLI hands over
    /// `repository_url` in `session_meta`, which is strictly better: it still
    /// resolves when the checkout has since been renamed, moved or deleted —
    /// which docs/tech-stack.md says is exactly when old memories matter most.
    pub repo: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub response: String,
    pub commands: Vec<Command>,
    pub files: Vec<FileRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub cmd: String,
    pub lang: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileRef {
    pub path: String,
    pub tool: String,
}

/// Non-fatal things a parse noticed. Surfaced by `tmem doctor` rather than
/// swallowed — silent failure is the enemy, and a parser that skips 40% of a
/// file without saying so is exactly the failure mode Phase 0 found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParseReport {
    pub records_total: usize,
    pub records_unparsable: usize,
    pub records_unknown_type: usize,
    pub prompts_found: usize,
    pub prompts_without_response: usize,
    /// Prompts skipped for want of a uuid or a usable timestamp.
    pub prompts_unusable: usize,
    pub api_errors_skipped: usize,
    pub sidechain_records: usize,
    /// Transcripts that were entirely subagent turns — one agent invocation
    /// each. Counted separately from `sidechain_records`, which counts subagent
    /// turns inlined in a main transcript.
    pub subagent_files: usize,
    /// Assistant records with no human prompt anywhere up the chain. Their text
    /// is real, and it is dropped — see the note at the attribution site.
    pub orphaned_records: usize,
    pub orphaned_chars: usize,
    pub unknown_types: Vec<String>,
}

pub trait Adapter: Sync {
    /// Stable name, stored in `exchanges.assistant`.
    fn name(&self) -> &'static str;

    /// The root of this assistant's transcript tree, honouring the adapter's
    /// own environment override so tests and second archives work the same way
    /// for every vendor.
    fn transcript_root(&self) -> anyhow::Result<PathBuf>;

    /// Every transcript file under `root`, and *only* transcript files.
    ///
    /// Not a glob in the pipeline, because the two vendors shipped so far
    /// disagree about both the shape of the tree and what else lives in it.
    /// The rule that matters is the same for both: a file this returns must be
    /// a conversation, because everything downstream reports a file it cannot
    /// parse as a problem with the archive.
    fn discover(&self, root: &Path) -> anyhow::Result<Vec<PathBuf>>;

    /// Tags that mark injected, non-prompt content inside a user-role record.
    /// Declared per adapter — see the module docs.
    fn injected_block_tags(&self) -> &'static [&'static str];

    /// Parse one whole transcript file into folded exchanges.
    ///
    /// Whole-file, not resume-from-offset: assembly is many-to-one (Phase 0
    /// finding 9) and records are not written in parent order (finding 8), so a
    /// byte offset is not a valid resume point. The watermark is a change
    /// detector, not a seek.
    /// Does this adapter recognise the file by its path alone?
    ///
    /// Only used by `tmem capture --path`, where a single file arrives with no
    /// tree around it to say whose it is. Deliberately narrow: a positive here
    /// must be a shape no other vendor writes.
    fn claims_path(&self, path: &Path) -> bool;

    fn parse(&self, source: &str, path: &str)
        -> anyhow::Result<(Vec<ParsedExchange>, ParseReport)>;
}

/// Every adapter, in the order they were added.
///
/// A registry rather than a hardcoded `ClaudeCode` at each call site: Phase 1
/// through 5 named the type directly in six places, which is six places to
/// forget when a vendor is added.
pub fn all() -> &'static [&'static dyn Adapter] {
    &[&claude_code::ClaudeCode, &codex::Codex, &pty_repl::PtyRepl]
}

pub fn by_name(name: &str) -> Option<&'static dyn Adapter> {
    all().iter().copied().find(|a| a.name() == name)
}

pub fn names() -> String {
    all()
        .iter()
        .map(|a| a.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which adapter owns a single file handed to `--path`.
///
/// Specific claims win; Claude Code is the fallback because its transcripts are
/// named `<uuid>.jsonl` and have no distinguishing shape to claim on. That
/// asymmetry is a fact about the formats, not a preference — and it is why
/// `--assistant` exists to override the guess.
pub fn for_path(path: &Path) -> &'static dyn Adapter {
    all()
        .iter()
        .copied()
        .find(|a| a.claims_path(path))
        .unwrap_or(&claude_code::ClaudeCode)
}

/// Every transcript file for every adapter, each paired with the adapter that
/// claims it. A missing tree is not an error — most machines have one vendor
/// installed, not all of them.
pub fn discover_all() -> anyhow::Result<Vec<(&'static dyn Adapter, PathBuf)>> {
    let mut out = Vec::new();
    for a in all() {
        let root = a.transcript_root()?;
        if !root.exists() {
            continue;
        }
        for f in a.discover(&root)? {
            out.push((*a, f));
        }
    }
    Ok(out)
}
