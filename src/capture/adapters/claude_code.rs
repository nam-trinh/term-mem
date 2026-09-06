//! Claude Code transcript adapter.
//!
//! Every rule here traces to a numbered finding in `docs/phases/phase-0.md`.
//! Five of the six traps found there fail *silently* — they produce a
//! plausible-looking archive that is wrong — so each has a fixture and a test
//! rather than a comment.

use super::{Adapter, Command, DedupKey, FileRef, ParseReport, ParsedExchange};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// Finding 1: eleven top-level record types in the Phase 0 sample, and this
/// project's own archive since grew two more (`cost-state`, `bridge-session`).
/// Allowlist, so type twelve is skipped rather than fatal.
const HANDLED_TYPES: &[&str] = &["user", "assistant", "system"];

/// Types we have seen and deliberately ignore. Anything outside both lists is
/// counted in the report so `doctor` can say the format moved.
const KNOWN_IGNORED_TYPES: &[&str] = &[
    "queue-operation",
    "attachment",
    "file-history-snapshot",
    "file-history-delta",
    "atis-latch",
    "ai-title",
    "last-prompt",
    "mode",
    "cost-state",
    "bridge-session",
    "summary",
];

/// Finding 2: the content-shape filter. Three `<ide_opened_file>` records in
/// the Phase 0 sample carried `promptSource: "sdk"` and `origin.kind: "human"`
/// — the metadata says human, the content is editor telemetry. Metadata alone
/// is insufficient.
const INJECTED_TAGS: &[&str] = &[
    "ide_opened_file",
    "ide_selection",
    "command-name",
    "command-message",
    "command-args",
    "local-command-caveat",
    "local-command-stdout",
    "local-command-stderr",
    "system-reminder",
    "user-memory-input",
];

/// Tools whose input is a command line.
const COMMAND_TOOLS: &[&str] = &["Bash", "BashOutput"];
/// Tools whose input names a file. Recorded as a path, never as a body.
const FILE_TOOLS: &[&str] = &["Edit", "Write", "Read", "MultiEdit", "NotebookEdit"];
/// Slash commands that control the conversation rather than ask anything.
/// They are not prompts, and — because `/clear` starts a fresh tree — they sit
/// at the root of one, where an unattributable assistant record would otherwise
/// come to rest and be presented as an answer to "/clear".
const CONTROL_COMMANDS: &[&str] = &["/clear", "/compact", "/resume", "/exit", "/quit", "/undo"];

/// Fence languages we treat as runnable.
const SHELL_LANGS: &[&str] = &["bash", "sh", "shell", "zsh", "console", "shell-session", ""];

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

/// Finding 3: `message.content` is polymorphic — a bare string or an array of
/// typed blocks. Both occur on `user` records.
impl Content {
    fn text(&self) -> String {
        match self {
            Content::Text(s) => s.clone(),
            Content::Blocks(bs) => bs
                .iter()
                .filter(|b| b.kind == "text")
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Message {
    model: Option<String>,
    content: Option<Content>,
}

#[derive(Debug, Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    parent_uuid: Option<String>,
    #[serde(rename = "logicalParentUuid")]
    logical_parent_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    is_compact_summary: Option<bool>,
    #[serde(rename = "isApiErrorMessage")]
    is_api_error: Option<bool>,
    #[serde(rename = "toolUseResult")]
    tool_use_result: Option<Value>,
    message: Option<Message>,
}

pub struct ClaudeCode;

impl ClaudeCode {
    /// Finding 2, and the Phase 1 blocking question.
    ///
    /// `promptSource` is a *positive* signal only. This project's own archive
    /// contains a genuine `origin.kind: "human"` record with the field absent
    /// (a slash-command invocation), which proves the field is optional even
    /// within one entrypoint — so gating on it would drop real records. The
    /// load-bearing discriminators are the negative ones plus the content shape.
    ///
    /// Returns the prompt text if this record is one, `None` otherwise.
    fn prompt_text(&self, r: &Record) -> Option<String> {
        if r.kind != "user" {
            return None;
        }
        if r.tool_use_result.is_some() {
            return None; // 24 of 41 user records in the Phase 0 sample
        }
        if r.is_meta == Some(true) || r.is_compact_summary == Some(true) {
            return None; // local-command caveats; the 14,872-char injected summary
        }
        if r.is_sidechain == Some(true) {
            return None; // subagent turns fold into the parent, never start one
        }
        let text = r.message.as_ref()?.content.as_ref()?.text();
        if text.trim().is_empty() {
            return None;
        }
        // A slash command is the one wrapped shape that *is* human intent, so
        // it is unwrapped rather than rejected. See docs/phases/phase-1.md:
        // Phase 0 counted these with the editor telemetry, but `/ship-phase` is
        // a request the user made and dropping it loses the exchange. The UI
        // no-ops (`/clear`, `/compact`) need no special case — they draw no
        // assistant response, so the empty-exchange rule already discards them.
        if let Some(cmd) = slash_command(&text) {
            let head = cmd.split_whitespace().next().unwrap_or_default();
            if CONTROL_COMMANDS.contains(&head) {
                return None;
            }
            return Some(cmd);
        }
        // Injected blocks are *stripped*, not grounds for rejection. Phase 1
        // rejected any record that opened with one, and Phase 2 found that this
        // loses real questions: the editor prepends `<ide_opened_file>` to the
        // very record that carries the prompt, as a separate text block, so
        // "does this record start with telemetry" and "is this record telemetry"
        // are different questions. See docs/phases/phase-2.md finding 1.
        let stripped = strip_injected_blocks(&text, self.injected_block_tags());
        let stripped = stripped.trim();
        if stripped.is_empty() {
            return None; // the record was nothing but injected content
        }
        // Note what is *not* consulted: `promptSource` and `origin.kind`. Both
        // are optional in practice (see above), so neither can gate ingest, and
        // a positive signal that cannot gate is not worth reading.
        Some(stripped.to_string())
    }

    fn is_human_prompt(&self, r: &Record) -> bool {
        self.prompt_text(r).is_some()
    }
}

/// `<command-message>x</command-message><command-name>/x</command-name>` and an
/// optional `<command-args>`, rendered back as the line the user typed.
fn slash_command(text: &str) -> Option<String> {
    let name = tag_body(text, "command-name")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let name = if name.starts_with('/') {
        name.to_string()
    } else {
        format!("/{name}")
    };
    match tag_body(text, "command-args").map(|a| a.trim().to_string()) {
        Some(args) if !args.is_empty() => Some(format!("{name} {args}")),
        _ => Some(name),
    }
}

fn tag_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

/// Remove every injected `<tag>…</tag>` span, keeping whatever the user typed
/// around it.
///
/// The Phase 1 rule tested only the *first* tag and rejected the whole record,
/// which is right for a record that is pure telemetry and wrong for the common
/// one that is telemetry followed by a question. Only a properly closed block
/// is removed — see the unclosed case below.
fn strip_injected_blocks(text: &str, tags: &[&str]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        if let Some(end) = after.find(['>', ' ', '\n', '\t']) {
            let tag = &after[..end];
            if tags.contains(&tag) {
                let close = format!("</{tag}>");
                if let Some(c) = after.find(&close) {
                    rest = &after[c + close.len()..];
                    continue;
                }
                // Unclosed. Treat it as ordinary text rather than swallowing
                // the remainder: "why does <ide_opened_file> appear here?" is a
                // question someone asked, and capture is the irreversible half.
                // A stray tag in a prompt is cosmetic; a truncated prompt is not.
            }
        }
        out.push('<');
        rest = after;
    }
    out
}

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn injected_block_tags(&self) -> &'static [&'static str] {
        INJECTED_TAGS
    }

    fn parse(&self, source: &str, path: &str) -> Result<(Vec<ParsedExchange>, ParseReport)> {
        let mut report = ParseReport::default();
        let mut records: Vec<Record> = Vec::new();
        // uuid -> effective parent. Finding 4: the parent chain breaks at
        // compaction, where `parentUuid` is null and the real link moves to
        // `logicalParentUuid`. Walk `parentUuid ?? logicalParentUuid`.
        let mut parent_of: HashMap<String, Option<String>> = HashMap::new();

        for (lineno, line) in source.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            report.records_total += 1;
            let r: Record = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    // Loud, but not fatal: one malformed line must not cost the
                    // rest of the file. Counted and surfaced by `doctor`.
                    report.records_unparsable += 1;
                    eprintln!("tmem: {path}:{}: unparsable record: {e}", lineno + 1);
                    continue;
                }
            };
            if let Some(u) = &r.uuid {
                let eff = r
                    .parent_uuid
                    .clone()
                    .or_else(|| r.logical_parent_uuid.clone());
                parent_of.insert(u.clone(), eff);
            }
            if !HANDLED_TYPES.contains(&r.kind.as_str()) {
                if !KNOWN_IGNORED_TYPES.contains(&r.kind.as_str()) {
                    report.records_unknown_type += 1;
                    if !report.unknown_types.contains(&r.kind) {
                        report.unknown_types.push(r.kind.clone());
                    }
                }
                continue;
            }
            if r.is_sidechain == Some(true) {
                report.sidechain_records += 1;
            }
            records.push(r);
        }

        // Second pass. Finding 8: records are not written in parent order —
        // children precede parents — so an exchange cannot be assembled by
        // walking the file top to bottom. Each assistant record is attributed
        // to the nearest human prompt above it *in the conversation tree*,
        // which is order-independent by construction.
        let session_id = records
            .iter()
            .find_map(|r| r.session_id.clone())
            .unwrap_or_else(|| {
                std::path::Path::new(path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string())
            });

        let mut out: Vec<ParsedExchange> = Vec::new();
        let mut prompt_idx: HashMap<String, usize> = HashMap::new();
        for r in &records {
            if !self.is_human_prompt(r) {
                continue;
            }
            report.prompts_found += 1;
            // Loud, but not fatal — the same rule the malformed-line handler
            // above follows. Erroring here would discard every other exchange
            // in the file over one unusable record, and capture is the half of
            // this system that cannot be redone later.
            let Some(uuid) = r.uuid.clone() else {
                eprintln!("tmem: {path}: user prompt record without a uuid; skipped");
                report.prompts_unusable += 1;
                continue;
            };
            let Some(ts) = parse_rfc3339_ms(r.timestamp.as_deref().unwrap_or_default()) else {
                eprintln!("tmem: {path}: prompt {uuid} has no usable timestamp; skipped");
                report.prompts_unusable += 1;
                continue;
            };
            prompt_idx.insert(uuid.clone(), out.len());
            out.push(ParsedExchange {
                session_id: session_id.clone(),
                thread_id: root_of(&uuid, &parent_of),
                // Claude Code's dedup key, declared by the adapter rather than
                // assumed by the pipeline. Codex CLI has no equivalent.
                source_key: DedupKey::Intrinsic(uuid).as_str(),
                ts_ms: ts,
                cwd: r.cwd.clone().unwrap_or_default(),
                git_branch: r.git_branch.clone(),
                model: None,
                prompt: self.prompt_text(r).unwrap_or_default(),
                response: String::new(),
                commands: Vec::new(),
                files: Vec::new(),
            });
        }

        // Attribute every assistant record, then fold in chronological order.
        let mut memo: HashMap<String, Option<usize>> = HashMap::new();
        let mut folded: Vec<(usize, i64, usize, &Record)> = Vec::new();
        for (i, r) in records.iter().enumerate() {
            if r.kind != "assistant" {
                continue;
            }
            // Finding 6: failed turns are stored in assistant shape. Skipping
            // them is also what keeps a retried prompt from duplicating — the
            // abandoned first prompt is then left with no assistant record and
            // drops out below.
            if r.is_api_error == Some(true) {
                report.api_errors_skipped += 1;
                continue;
            }
            let Some(uuid) = r.uuid.as_deref() else {
                continue;
            };
            let Some(owner) = owner_prompt(uuid, &parent_of, &prompt_idx, &mut memo) else {
                // No human prompt anywhere up the chain. Real in this archive:
                // a resumed session's transcript can hold assistant records
                // whose prompts were never written to it. Dropped rather than
                // hung off the nearest prompt-shaped ancestor, which would
                // present them as an answer to a question nobody asked — but
                // counted, so the loss is reported instead of silent.
                if let Some(m) = &r.message {
                    if let Some(c) = &m.content {
                        let n = c.text().trim().len();
                        if n > 0 {
                            report.orphaned_records += 1;
                            report.orphaned_chars += n;
                        }
                    }
                }
                continue;
            };
            let ts =
                parse_rfc3339_ms(r.timestamp.as_deref().unwrap_or_default()).unwrap_or(i64::MAX);
            folded.push((owner, ts, i, r));
        }
        folded.sort_by_key(|(owner, ts, i, _)| (*owner, *ts, *i));

        let mut answered = vec![false; out.len()];
        for (owner, _, _, r) in folded {
            let ex = &mut out[owner];
            answered[owner] = true;
            let Some(msg) = &r.message else { continue };
            if ex.model.is_none() {
                ex.model = msg.model.clone();
            }
            if ex.cwd.is_empty() {
                ex.cwd = r.cwd.clone().unwrap_or_default();
            }
            if ex.git_branch.is_none() {
                ex.git_branch = r.git_branch.clone();
            }
            let Some(content) = &msg.content else {
                continue;
            };

            // Finding 9: assembly is many-to-one — roughly six assistant records
            // per prompt, one per tool round trip, folding into one row.
            match content {
                Content::Text(t) => append_response(ex, t),
                Content::Blocks(blocks) => {
                    for b in blocks {
                        match b.kind.as_str() {
                            "text" => {
                                if let Some(t) = &b.text {
                                    append_response(ex, t);
                                }
                            }
                            // `thinking` is never stored. Phase 0 found all 23
                            // blocks empty on disk and the Codex survey found the
                            // same; the rule stays as defense-in-depth against
                            // that changing.
                            "thinking" | "redacted_thinking" => {}
                            "tool_use" => mine_tool_use(b, ex),
                            _ => {}
                        }
                    }
                }
            }
        }

        // A prompt with no assistant record at all is an interrupted turn, or
        // one whose only reply was an API error and which the user re-sent.
        // Writing it would leave a permanently empty row in the archive; a
        // later ingest of the same file picks it up if a response arrives.
        let mut kept: Vec<ParsedExchange> = Vec::new();
        for (i, ex) in out.into_iter().enumerate() {
            if answered[i] {
                kept.push(ex);
            } else {
                report.prompts_without_response += 1;
            }
        }
        let mut out = kept;
        out.sort_by_key(|e| e.ts_ms);

        // Fenced blocks in prose are the other command source — scenario 1's
        // ffmpeg line arrives that way, not through a tool call.
        for ex in out.iter_mut() {
            let mut seen: Vec<String> = ex.commands.iter().map(|c| c.cmd.clone()).collect();
            for c in extract_fenced_commands(&ex.response) {
                if !seen.contains(&c.cmd) {
                    seen.push(c.cmd.clone());
                    ex.commands.push(c);
                }
            }
        }

        Ok((out, report))
    }
}

fn append_response(ex: &mut ParsedExchange, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if !ex.response.is_empty() {
        ex.response.push_str("\n\n");
    }
    ex.response.push_str(text);
}

/// Walk up the conversation tree to the nearest human prompt. Memoised, since
/// six assistant records per prompt each walk the same chain.
fn owner_prompt(
    uuid: &str,
    parent_of: &HashMap<String, Option<String>>,
    prompts: &HashMap<String, usize>,
    memo: &mut HashMap<String, Option<usize>>,
) -> Option<usize> {
    let mut chain: Vec<String> = Vec::new();
    let mut cur = uuid.to_string();
    let found = loop {
        if let Some(hit) = memo.get(&cur) {
            break *hit;
        }
        if let Some(i) = prompts.get(&cur) {
            break Some(*i);
        }
        if chain.contains(&cur) {
            break None; // a cycle; refuse to loop rather than hang
        }
        chain.push(cur.clone());
        match parent_of.get(&cur) {
            Some(Some(p)) => cur = p.clone(),
            _ => break None,
        }
    };
    for u in chain {
        memo.insert(u, found);
    }
    found
}

fn mine_tool_use(b: &Block, ex: &mut ParsedExchange) {
    let (Some(name), Some(input)) = (&b.name, &b.input) else {
        return;
    };
    if COMMAND_TOOLS.contains(&name.as_str()) {
        if let Some(cmd) = input.get("command").and_then(Value::as_str) {
            if !cmd.trim().is_empty() {
                ex.commands.push(Command {
                    cmd: cmd.trim().to_string(),
                    lang: Some("bash".into()),
                });
            }
        }
    } else if FILE_TOOLS.contains(&name.as_str()) {
        for key in ["file_path", "notebook_path", "path"] {
            if let Some(p) = input.get(key).and_then(Value::as_str) {
                ex.files.push(FileRef {
                    path: p.to_string(),
                    tool: name.clone(),
                });
                break;
            }
        }
    }
}

/// Pull runnable lines out of fenced code blocks. Comment lines, blank lines
/// and a leading `$ ` prompt marker are dropped; everything else in a
/// shell-flavoured fence is a candidate command.
fn extract_fenced_commands(text: &str) -> Vec<Command> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut lang = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if in_fence {
                in_fence = false;
                lang.clear();
            } else {
                in_fence = true;
                lang = rest.split_whitespace().next().unwrap_or("").to_lowercase();
            }
            continue;
        }
        if !in_fence || !SHELL_LANGS.contains(&lang.as_str()) {
            continue;
        }
        let mut cmd = line.trim();
        if let Some(rest) = cmd.strip_prefix("$ ") {
            cmd = rest.trim();
        }
        if cmd.is_empty() || cmd.starts_with('#') {
            continue;
        }
        out.push(Command {
            cmd: cmd.to_string(),
            lang: Some(if lang.is_empty() {
                "sh".into()
            } else {
                lang.clone()
            }),
        });
    }
    out
}

/// Walk to the conversation-tree root. Finding 5: one session file can hold
/// several conversations, because `/clear` starts a fresh tree under the same
/// `sessionId`. Grouping `--session` on `session_id` merges unrelated threads.
fn root_of(uuid: &str, parent_of: &HashMap<String, Option<String>>) -> String {
    let mut cur = uuid.to_string();
    let mut seen = vec![cur.clone()];
    loop {
        match parent_of.get(&cur) {
            Some(Some(p)) if !seen.contains(p) => {
                cur = p.clone();
                seen.push(cur.clone());
            }
            // A parent we never saw is a forward reference into a truncated
            // file, not corruption; stop at the deepest ancestor we have.
            _ => return cur,
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fff]Z` to unix milliseconds. Hand-rolled rather than
/// pulling in a date crate: the format is fixed, and an unparsable timestamp
/// must be loud (returns `None`, and the caller errors) rather than defaulting
/// to the epoch and silently sorting an exchange to the start of time.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut ms = 0i64;
    let rest = &s[19..];
    let rest = if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        let n = digits.len();
        let v: i64 = digits.parse().ok()?;
        ms = match n {
            0 => 0,
            1 => v * 100,
            2 => v * 10,
            3 => v,
            _ => v / 10i64.pow(n as u32 - 3),
        };
        &rest[1 + n..]
    } else {
        rest
    };
    // Offsets other than Z are not emitted by Claude Code; reject rather than
    // guess, so a format change is loud.
    let offset_s = match rest {
        "Z" | "z" | "" => 0i64,
        r if r.len() == 6 && (r.starts_with('+') || r.starts_with('-')) => {
            let sign = if r.starts_with('-') { -1 } else { 1 };
            let oh: i64 = r.get(1..3)?.parse().ok()?;
            let om: i64 = r.get(4..6)?.parse().ok()?;
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    Some(((days * 86400 + h * 3600 + mi * 60 + sec - offset_s) * 1000) + ms)
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude_code/");
        std::fs::read_to_string(format!("{p}{name}")).expect("fixture")
    }

    fn parse(name: &str) -> (Vec<ParsedExchange>, ParseReport) {
        ClaudeCode.parse(&fixture(name), name).expect("parse")
    }

    // ── Phase 0 finding 1 ────────────────────────────────────────────────
    #[test]
    fn unknown_record_types_are_skipped_not_fatal() {
        let (ex, report) = parse("finding-01-record-types.jsonl");
        assert_eq!(ex.len(), 1, "one prompt among thirteen record types");
        // The twelfth type is counted so `doctor` can say the format moved.
        assert_eq!(report.unknown_types, vec!["some-future-record-type"]);
        assert_eq!(report.records_unknown_type, 1);
        assert_eq!(report.records_unparsable, 0);
    }

    // ── Phase 0 finding 2 — the serious one ──────────────────────────────
    #[test]
    fn only_genuine_prompts_survive_the_discriminator() {
        let (ex, report) = parse("finding-02-user-records.jsonl");
        // Eight user records; two typed questions. The `/clear` and `/compact`
        // echoes are conversation control, not requests.
        assert_eq!(report.prompts_found, 2);
        assert_eq!(ex.len(), 2);
        assert!(ex[0].prompt.starts_with("why is the signature check"));
        assert!(ex[1].prompt.starts_with("what tolerance"));
        // Nothing injected leaked into the archive.
        for e in &ex {
            assert!(!e.prompt.contains("ide_opened_file"));
            assert!(!e.prompt.contains("command-name"));
            assert!(!e.prompt.contains("Caveat:"));
            assert!(!e.prompt.contains("continued from a previous conversation"));
        }
    }

    /// The Phase 1 blocking question, settled empirically: a `promptSource`-less
    /// record with `origin.kind: "human"` exists, so requiring the field would
    /// drop real prompts. Fixture record `u8` is exactly that shape.
    #[test]
    fn prompt_source_is_not_required() {
        let (ex, _) = parse("finding-02-user-records.jsonl");
        assert!(
            ex.iter().any(|e| e.prompt.starts_with("what tolerance")),
            "a prompt with promptSource absent must still be captured"
        );
    }

    /// The Phase 2 finding, and the reason 19 KB of real answers were missing
    /// from a four-day archive: the editor prepends `<ide_opened_file>` to the
    /// record that carries the prompt, so Phase 1's "starts with an injected
    /// tag" rule rejected the question and orphaned its response.
    ///
    /// Written from the promise ("a question the user asked is captured"),
    /// which is the lesson of phase-1.md finding 9.
    #[test]
    fn a_prompt_behind_editor_telemetry_is_still_a_prompt() {
        let (ex, report) = parse("finding-p2-injected-prefix.jsonl");
        assert_eq!(
            report.orphaned_records, 0,
            "no response should be left unattributable: {report:?}"
        );
        assert_eq!(ex.len(), 2);
        assert_eq!(
            ex[0].prompt,
            "I have 4 mp4 files I need to join into one. Same codec, same resolution. \
             Don't want to re-encode."
        );
        assert!(!ex[0].prompt.contains("ide_opened_file"));
        // A trailing injected block goes the same way as a leading one.
        assert_eq!(ex[1].prompt, "why -safe 0?");
    }

    /// The other half of the rule: a record that is *nothing but* injected
    /// content is still not a prompt. Phase 1 got this half right and it must
    /// not regress — `<ide_opened_file>` alone opens no exchange.
    #[test]
    fn a_record_that_is_only_telemetry_is_still_rejected() {
        let (ex, _) = parse("finding-p2-injected-prefix.jsonl");
        assert!(
            !ex.iter().any(|e| e.prompt.contains("files.txt in the IDE")),
            "a pure-telemetry record must not become an exchange"
        );
    }

    #[test]
    fn stripping_leaves_surrounding_text_and_unknown_tags_alone() {
        let tags = ["ide_opened_file", "system-reminder"];
        assert_eq!(
            strip_injected_blocks("<ide_opened_file>x</ide_opened_file>real question", &tags),
            "real question"
        );
        assert_eq!(
            strip_injected_blocks("before <system-reminder>x</system-reminder> after", &tags),
            "before  after"
        );
        // A tag we do not know is ordinary text: a user asking about `<div>`
        // must get their question back intact.
        assert_eq!(
            strip_injected_blocks("why does <div> not center?", &tags),
            "why does <div> not center?"
        );
        // An unclosed injected block is left alone rather than swallowing the
        // rest of the prompt.
        assert_eq!(
            strip_injected_blocks("keep <system-reminder>then this", &tags),
            "keep <system-reminder>then this"
        );
    }

    /// Phase 0 counted slash-command records with the editor telemetry. They
    /// are not the same thing: `/ship-phase` is a request the user made, and
    /// dropping it loses the exchange. See docs/phases/phase-1.md finding 2.
    #[test]
    fn a_slash_command_is_a_prompt_not_an_echo() {
        let (ex, report) = parse("slash-command.jsonl");
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].prompt, "/ship-phase 1");
        assert_eq!(ex[0].response, "Starting phase 1.");
        // `/clear` is conversation control, not a request.
        assert_eq!(report.prompts_found, 1);
    }

    /// The trap this fixture records: a resumed transcript can hold assistant
    /// records whose human prompt was never written to it. Walking the tree
    /// lands them on the nearest prompt-shaped ancestor — the `/clear` echo —
    /// where they would be presented as an answer to "/clear".
    #[test]
    fn unattributable_responses_are_dropped_loudly_not_misattributed() {
        let (ex, report) = parse("orphaned-response.jsonl");
        assert_eq!(ex.len(), 1, "only the turn that has a prompt");
        assert_eq!(ex[0].prompt, "a real question, asked and answered");
        assert_eq!(ex[0].response, "a real answer");
        assert!(
            !ex[0].response.contains("not in this file"),
            "an orphan must not be folded into an unrelated exchange"
        );
        // Dropped, but counted — `tmem capture` prints this.
        assert_eq!(report.orphaned_records, 2);
        assert!(report.orphaned_chars > 80);
    }

    #[test]
    fn conversation_control_commands_are_not_prompts() {
        let (ex, _) = parse("orphaned-response.jsonl");
        assert!(ex.iter().all(|e| e.prompt != "/clear"));
    }

    #[test]
    fn slash_command_parsing() {
        assert_eq!(
            slash_command("<command-name>/clear</command-name>").as_deref(),
            Some("/clear")
        );
        assert_eq!(
            slash_command("<command-message>x</command-message>\n<command-name>ship-phase</command-name>\n<command-args>2</command-args>").as_deref(),
            Some("/ship-phase 2")
        );
        assert_eq!(slash_command("just a sentence"), None);
    }

    /// Kept from Phase 1, where the same cases guarded the anchored *rejection*
    /// rule. The rule became a strip in Phase 2; what must not change is that a
    /// prompt merely mentioning a tag survives intact.
    #[test]
    fn injected_tag_matching_does_not_eat_prompts_that_mention_a_tag() {
        assert_eq!(
            strip_injected_blocks("<ide_opened_file>x</ide_opened_file>", INJECTED_TAGS),
            ""
        );
        assert_eq!(
            strip_injected_blocks("  <command-name>/clear</command-name>", INJECTED_TAGS),
            "  "
        );
        assert_eq!(
            strip_injected_blocks("why does <ide_opened_file> appear here?", INJECTED_TAGS),
            "why does <ide_opened_file> appear here?"
        );
        assert_eq!(
            strip_injected_blocks("<div> is not one of ours", INJECTED_TAGS),
            "<div> is not one of ours"
        );
    }

    // ── Phase 0 finding 3 ────────────────────────────────────────────────
    #[test]
    fn content_may_be_a_string_or_an_array() {
        let (ex, _) = parse("finding-03-polymorphic-content.jsonl");
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].prompt, "content as a bare string, not an array");
        assert_eq!(ex[1].prompt, "content as an array of typed blocks");
    }

    // ── Phase 0 finding 4 ────────────────────────────────────────────────
    #[test]
    fn thread_walk_survives_a_compaction_boundary() {
        let (ex, _) = parse("finding-04-compaction.jsonl");
        assert_eq!(ex.len(), 2);
        // `compact_boundary` has parentUuid: null and moves the real link to
        // logicalParentUuid. Walking parentUuid alone would give the second
        // exchange its own thread and truncate `show --session` silently.
        assert_eq!(
            ex[0].thread_id, ex[1].thread_id,
            "both turns belong to one conversation across the compaction"
        );
        assert_eq!(ex[0].thread_id, "u1");
    }

    // ── Phase 0 finding 5 ────────────────────────────────────────────────
    #[test]
    fn one_session_file_can_hold_several_conversations() {
        let (ex, _) = parse("finding-05-two-threads.jsonl");
        assert_eq!(ex.len(), 3);
        assert_eq!(
            ex[0].session_id, ex[2].session_id,
            "same file, same session id"
        );
        assert_eq!(ex[0].thread_id, ex[1].thread_id);
        assert_ne!(
            ex[0].thread_id, ex[2].thread_id,
            "/clear starts a new tree; grouping on session_id would merge them"
        );
    }

    // ── Phase 0 finding 6, and the retry it also fixes ───────────────────
    #[test]
    fn api_errors_are_skipped_and_a_retry_does_not_duplicate() {
        let (ex, report) = parse("finding-06-api-error-retry.jsonl");
        assert_eq!(report.api_errors_skipped, 1);
        assert_eq!(
            ex.len(),
            1,
            "the identical re-sent prompt must not produce a second row"
        );
        assert!(!ex[0].response.contains("OAuth session expired"));
        assert_eq!(ex[0].response, "Batch it, with a checkpoint table.");
        // The abandoned first prompt is dropped because skipping the error left
        // it with no assistant record at all.
        assert_eq!(report.prompts_found, 2);
        assert_eq!(report.prompts_without_response, 1);
    }

    // ── Phase 0 finding 8 ────────────────────────────────────────────────
    #[test]
    fn records_written_out_of_parent_order_still_resolve() {
        let (ex, _) = parse("finding-08-out-of-order.jsonl");
        assert_eq!(ex.len(), 2);
        assert_eq!(
            ex[0].thread_id, ex[1].thread_id,
            "a parent appearing later in the file is a forward reference, not corruption"
        );
        assert_eq!(ex[0].thread_id, "root");
    }

    // ── Phase 0 finding 9, and finding 7's two decisions ─────────────────
    #[test]
    fn many_assistant_records_fold_into_one_exchange() {
        let (ex, _) = parse("finding-09-many-to-one.jsonl");
        assert_eq!(ex.len(), 1, "five assistant records, one prompt, one row");
        let e = &ex[0];
        assert!(e.response.contains("Step 1."));
        assert!(e.response.contains("Step 4."));
        assert!(e.response.contains("holds all four clips"));
    }

    #[test]
    fn tool_use_is_mined_not_stored() {
        let (ex, _) = parse("finding-09-many-to-one.jsonl");
        let e = &ex[0];
        let cmds: Vec<&str> = e.commands.iter().map(|c| c.cmd.as_str()).collect();
        assert!(cmds.contains(&"ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4"));
        assert!(cmds.contains(&"ffprobe -v error out.mp4"));
        // Edit and Read become path references, never bodies.
        assert_eq!(e.files.len(), 2);
        assert!(e.files.iter().all(|f| f.path.ends_with("files.txt")));
        assert!(
            !e.response.contains("old_string"),
            "no tool_use payload in the response"
        );
    }

    #[test]
    fn a_command_is_recorded_once_however_many_ways_it_appears() {
        // The ffmpeg line arrives twice: as a Bash tool_use and inside a fenced
        // block in the prose. One row, not two.
        let (ex, _) = parse("finding-09-many-to-one.jsonl");
        let n = ex[0]
            .commands
            .iter()
            .filter(|c| c.cmd.starts_with("ffmpeg -f concat"))
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn thinking_blocks_are_never_stored() {
        let (ex, _) = parse("finding-09-many-to-one.jsonl");
        assert!(!ex[0].response.contains("signature"));
        assert!(!ex[0].response.contains("thinking"));
    }

    #[test]
    fn fenced_blocks_yield_commands() {
        let cmds = extract_fenced_commands(
            "try this:\n\n```bash\n# join them\n$ ffmpeg -i a.mp4 out.mp4\n\nls -la\n```\n\nand not this:\n\n```python\nprint('no')\n```",
        );
        let got: Vec<&str> = cmds.iter().map(|c| c.cmd.as_str()).collect();
        assert_eq!(got, vec!["ffmpeg -i a.mp4 out.mp4", "ls -la"]);
    }

    // ── beyond the numbered findings ─────────────────────────────────────
    #[test]
    fn an_interrupted_turn_writes_no_empty_row() {
        let (ex, report) = parse("interrupted-turn.jsonl");
        assert_eq!(ex.len(), 1);
        assert_eq!(report.prompts_without_response, 1);
    }

    #[test]
    fn sidechain_turns_fold_into_the_parent_exchange() {
        let (ex, report) = parse("sidechain.jsonl");
        assert_eq!(report.sidechain_records, 2);
        assert_eq!(
            ex.len(),
            1,
            "a subagent prompt must not open an exchange of its own"
        );
        // Attributed to the parent: the subagent's command is on the parent row.
        assert!(ex[0].commands.iter().any(|c| c.cmd.contains("max_retries")));
        assert!(ex[0].response.contains("Found it in retry.py"));
        assert!(ex[0].response.contains("caps at five attempts"));
    }

    #[test]
    fn one_malformed_line_does_not_cost_the_file() {
        let (ex, report) = parse("malformed-line.jsonl");
        assert_eq!(report.records_unparsable, 1);
        assert_eq!(ex.len(), 1);
        assert!(ex[0].response.contains("after the damage"));
    }

    #[test]
    fn timestamps_parse_or_fail_loudly() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-03-03T14:22:07.486Z"),
            Some(1772547727486)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-03-03T14:22:07Z"),
            Some(1772547727000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-03-03T14:22:07.4Z"),
            Some(1772547727400)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-03-03T15:22:07+01:00"),
            Some(1772547727000)
        );
        // No silent default to the epoch, which would sort the exchange to the
        // start of time and look like a very old memory.
        assert_eq!(parse_rfc3339_ms(""), None);
        assert_eq!(parse_rfc3339_ms("yesterday"), None);
        assert_eq!(parse_rfc3339_ms("2026-13-03T14:22:07Z"), None);
        assert_eq!(parse_rfc3339_ms("2026-03-03 14:22:07"), None);
    }

    #[test]
    fn a_cycle_in_the_parent_chain_terminates() {
        let mut m = HashMap::new();
        m.insert("a".to_string(), Some("b".to_string()));
        m.insert("b".to_string(), Some("a".to_string()));
        assert_eq!(root_of("a", &m), "b");
    }
}
