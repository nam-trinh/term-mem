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
//! So both are properties of the adapter, not of the ingest pipeline.

pub mod claude_code;

/// How an adapter identifies a record for idempotency purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupKey {
    /// A stable per-record identifier the format supplies. Claude Code's `uuid`.
    Intrinsic(String),
    /// Position in an append-only file. The only key Codex CLI can offer; a
    /// genuinely weaker guarantee, and one that holds only while the file is
    /// never rewritten in place.
    // Unconstructed until Phase 6 adds the Codex adapter; declared now because
    // it is the finding that shapes this interface, not an afterthought.
    #[allow(dead_code)]
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
    pub api_errors_skipped: usize,
    pub sidechain_records: usize,
    /// Assistant records with no human prompt anywhere up the chain. Their text
    /// is real, and it is dropped — see the note at the attribution site.
    pub orphaned_records: usize,
    pub orphaned_chars: usize,
    pub unknown_types: Vec<String>,
}

pub trait Adapter {
    /// Stable name, stored in `exchanges.assistant`.
    fn name(&self) -> &'static str;

    /// Tags that mark injected, non-prompt content inside a user-role record.
    /// Declared per adapter — see the module docs.
    fn injected_block_tags(&self) -> &'static [&'static str];

    /// Parse one whole transcript file into folded exchanges.
    ///
    /// Whole-file, not resume-from-offset: assembly is many-to-one (Phase 0
    /// finding 9) and records are not written in parent order (finding 8), so a
    /// byte offset is not a valid resume point. The watermark is a change
    /// detector, not a seek.
    fn parse(&self, source: &str, path: &str)
        -> anyhow::Result<(Vec<ParsedExchange>, ParseReport)>;
}
