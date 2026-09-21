//! Codex CLI — the second vendor, and the one that shaped the parser interface.
//!
//! [docs/phases/codex-cli-format.md](../../../docs/phases/codex-cli-format.md)
//! surveyed this format before the adapter existed. Its headline holds: the
//! transcript is complete and structured, so tier 1 is right for a second
//! assistant. What it costs is every assumption Claude Code let us make.
//!
//! * **No record identity.** No `uuid`, no `parentUuid`, no `id` on any
//!   `response_item`. The dedup key is positional — `@<line>` in an append-only
//!   file — which is a genuinely weaker guarantee and is why [`DedupKey`] is an
//!   enum rather than a string.
//! * **Two overlapping streams.** `event_msg`/`agent_message` repeats
//!   `response_item`/`message` verbatim. A parser that dispatches on
//!   `payload.type` without first filtering the *top-level* `type` counts every
//!   response twice, and both copies are real text, so nothing looks wrong.
//! * **Injected blocks are stripped, not rejected.** `<environment_context>` is
//!   prepended to the same string as the real prompt rather than occupying its
//!   own record — the opposite of the Claude Code case, where the injected
//!   record *is* the whole record.
//!
//! Assembly is many-to-one exactly as in Claude Code, but the folding rule is
//! positional rather than tree-shaped: there are no parent pointers to walk, so
//! an assistant message belongs to the most recent user message above it in the
//! file. That is only sound because the file is append-only.

use super::{Adapter, Command, DedupKey, FileRef, ParseReport, ParsedExchange};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Codex;

/// Blocks Codex prepends to a user message. Finding 3: these share the string
/// with the real prompt, so they are cut out and what remains is the question.
const INJECTED_TAGS: &[&str] = &[
    "environment_context",
    "turn_aborted",
    "subagent_notification",
    "user_instructions",
];

/// Top-level `type` values that are conversation. Everything else is either the
/// duplicate UI stream or per-turn bookkeeping.
const CONVERSATION: &str = "response_item";

/// Top-level types we know about and deliberately skip. Anything outside both
/// lists is reported as unknown, because an unrecognised record is how a format
/// change announces itself.
const KNOWN_IGNORED: &[&str] = &[
    "event_msg",     // finding 2: a verbatim duplicate of response_item
    "turn_context",  // per-turn settings; read for `model`, never for content
    "session_meta",  // finding 5: the metadata gift, read separately
    "compacted",     // finding 6: a history rewrite, not new conversation
    "response.done", // usage accounting, new since the survey
    "event",         // seen in newer builds alongside event_msg
];

#[derive(Debug, Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<Value>,
}

#[derive(Debug, Default, Clone)]
struct Meta {
    cwd: Option<String>,
    branch: Option<String>,
    repository_url: Option<String>,
    model: Option<String>,
    session_id: Option<String>,
}

impl Adapter for Codex {
    fn name(&self) -> &'static str {
        "codex-cli"
    }

    fn transcript_root(&self) -> Result<PathBuf> {
        crate::paths::codex_sessions_dir()
    }

    /// `sessions/YYYY/MM/DD/rollout-*.jsonl`, and nothing else.
    ///
    /// The survey did not raise discovery and it turns out to be the sharper
    /// trap. `~/.codex` holds `session_index.jsonl` (a list of thread names,
    /// with no `type` field on any line) and, on this machine, a plugin's
    /// `responses.jsonl` fixture. A `**/*.jsonl` sweep ingests both, and
    /// because the pipeline reports anything it cannot parse, the user gets a
    /// permanent complaint about files that were never transcripts.
    ///
    /// So: only under `sessions/`, only `rollout-*.jsonl`. `archived_sessions/`
    /// is deliberately left out — it is Codex's own retention decision and
    /// re-ingesting what a user archived is not obviously wanted.
    fn discover(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        walk(root, 0, &mut out)?;
        out.sort();
        Ok(out)
    }

    /// `rollout-*.jsonl` is a Codex name and nothing else writes it.
    fn claims_path(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
    }

    fn injected_block_tags(&self) -> &'static [&'static str] {
        INJECTED_TAGS
    }

    fn parse(&self, source: &str, path: &str) -> Result<(Vec<ParsedExchange>, ParseReport)> {
        let mut report = ParseReport::default();
        let mut meta = Meta::default();
        let mut out: Vec<ParsedExchange> = Vec::new();
        // The index of the exchange an assistant message belongs to. Positional
        // because there is nothing else: no parent pointers exist to walk.
        let mut current: Option<usize> = None;

        for (i, line) in source.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            report.records_total += 1;
            let rec: Record = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(_) => {
                    report.records_unparsable += 1;
                    continue;
                }
            };
            let kind = rec.kind.as_deref().unwrap_or("");

            if kind == "session_meta" {
                absorb_meta(&mut meta, rec.payload.as_ref());
                continue;
            }
            if kind == "turn_context" {
                // Finding 5: `cwd` and `model` are snapshotted per turn, so
                // they are read per turn rather than once per file.
                if let Some(p) = rec.payload.as_ref() {
                    if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                        meta.cwd = Some(c.to_string());
                    }
                    if let Some(m) = p.get("model").and_then(Value::as_str) {
                        meta.model = Some(m.to_string());
                    }
                }
                continue;
            }
            if kind != CONVERSATION {
                if !KNOWN_IGNORED.contains(&kind) {
                    report.records_unknown_type += 1;
                    let label = if kind.is_empty() {
                        "<no type field>".to_string()
                    } else {
                        kind.to_string()
                    };
                    if !report.unknown_types.contains(&label) {
                        report.unknown_types.push(label);
                    }
                }
                continue;
            }

            let Some(payload) = rec.payload.as_ref() else {
                report.records_unparsable += 1;
                continue;
            };
            let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
            let ts = rec
                .timestamp
                .as_deref()
                .and_then(super::claude_code::parse_rfc3339_ms);

            match ptype {
                "message" => {
                    let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                    match role {
                        // Finding 3: the permissions/sandbox system prompt.
                        "developer" | "system" => continue,
                        "user" => {
                            let text = content_text(payload);
                            let stripped =
                                super::claude_code::strip_injected_blocks(&text, INJECTED_TAGS);
                            let stripped = stripped.trim();
                            if stripped.is_empty() {
                                continue; // the record was nothing but injected context
                            }
                            report.prompts_found += 1;
                            let Some(ts) = ts else {
                                eprintln!(
                                    "tmem: {path}: user message at line {} has no usable \
                                     timestamp; skipped",
                                    i + 1
                                );
                                report.prompts_unusable += 1;
                                continue;
                            };
                            current = Some(out.len());
                            out.push(ParsedExchange {
                                session_id: meta.session_id.clone().unwrap_or_else(|| {
                                    Path::new(path)
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| path.to_string())
                                }),
                                // No tree, so no root to walk to. One file is
                                // one thread: Codex has no `/clear` that starts
                                // a fresh conversation inside the same file.
                                thread_id: meta
                                    .session_id
                                    .clone()
                                    .unwrap_or_else(|| path.to_string()),
                                source_key: DedupKey::Positional { line: i + 1 }.as_str(),
                                ts_ms: ts,
                                cwd: meta.cwd.clone().unwrap_or_default(),
                                // Finding 5: the one thing Codex gives that
                                // Claude Code does not.
                                repo: meta.repository_url.as_deref().and_then(repo_name_from_url),
                                git_branch: meta.branch.clone(),
                                model: meta.model.clone(),
                                prompt: stripped.to_string(),
                                response: String::new(),
                                commands: Vec::new(),
                                files: Vec::new(),
                            });
                        }
                        "assistant" => {
                            let Some(ex) = current.and_then(|c| out.get_mut(c)) else {
                                // An assistant message before any user message.
                                // Same rule as Claude Code's orphans: counted
                                // and dropped, never misattributed.
                                let n = content_text(payload).trim().len();
                                if n > 0 {
                                    report.orphaned_records += 1;
                                    report.orphaned_chars += n;
                                }
                                continue;
                            };
                            let text = content_text(payload);
                            if !text.trim().is_empty() {
                                if !ex.response.is_empty() {
                                    ex.response.push_str("\n\n");
                                }
                                ex.response.push_str(text.trim());
                            }
                        }
                        _ => {}
                    }
                }
                // The mined artefacts. Same rule as Claude Code: the raw block
                // is never stored, so anything not extracted here is gone.
                "function_call" | "custom_tool_call" => {
                    if let Some(ex) = current.and_then(|c| out.get_mut(c)) {
                        mine_call(payload, ex);
                    }
                }
                "reasoning" => {
                    // Finding 4: empty on disk, in both vendors. Nothing to do,
                    // and nothing to store even if it were not.
                }
                _ => {}
            }
        }

        // Finding 2's other half, made checkable: if this file's `event_msg`
        // stream had been ingested too, the response text would be duplicated.
        // Nothing asserts that here — the filter above is the guard — but the
        // count is reported so a regression shows up as a number.
        report.prompts_without_response = out.iter().filter(|e| e.response.is_empty()).count();
        out.retain(|e| !e.response.trim().is_empty());
        Ok((out, report))
    }
}

/// Recurse into `sessions/YYYY/MM/DD/`, at most four levels, collecting only
/// `rollout-*.jsonl`.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if depth > 4 || !dir.is_dir() {
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            walk(&p, depth + 1, out)?;
        } else if p.extension().is_some_and(|x| x == "jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
        {
            out.push(p);
        }
    }
    Ok(())
}

/// `https://github.com/acme/billing-api.git` → `billing-api`.
///
/// The bare name, because that is what `--repo` compares against and what
/// `resolve_repo` produces for the other adapter. Storing the URL would make
/// the same checkout un-searchable depending on which assistant recorded it.
fn repo_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let last = trimmed.rsplit(['/', ':']).next()?;
    (!last.is_empty()).then(|| last.to_string())
}

fn absorb_meta(meta: &mut Meta, payload: Option<&Value>) {
    let Some(p) = payload else { return };
    if let Some(c) = p.get("cwd").and_then(Value::as_str) {
        meta.cwd = Some(c.to_string());
    }
    if let Some(id) = p.get("id").and_then(Value::as_str) {
        meta.session_id = Some(id.to_string());
    }
    // Finding 5, and the one thing Codex gives that Claude Code does not.
    // Every field is individually optional: one session in the sample has a
    // `git` object with only `repository_url` in it.
    if let Some(g) = p.get("git") {
        if let Some(b) = g.get("branch").and_then(Value::as_str) {
            meta.branch = Some(b.to_string());
        }
        if let Some(u) = g.get("repository_url").and_then(Value::as_str) {
            meta.repository_url = Some(u.to_string());
        }
    }
}

/// `content: [{type, text}]`, joined. Codex uses `input_text` for user records
/// and `output_text` for assistant ones; both are read, because a parser that
/// knows only one of them silently drops half the conversation.
fn content_text(payload: &Value) -> String {
    let Some(items) = payload.get("content").and_then(Value::as_array) else {
        return payload
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    };
    let mut parts = Vec::new();
    for it in items {
        if let Some(t) = it.get("text").and_then(Value::as_str) {
            if !t.is_empty() {
                parts.push(t);
            }
        }
    }
    parts.join("\n")
}

/// Pull the command line and any file path out of a tool call.
fn mine_call(payload: &Value, ex: &mut ParsedExchange) {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // `arguments` is a JSON *string*, not an object.
    let args: Value = payload
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .or_else(|| payload.get("input").cloned())
        .unwrap_or(Value::Null);

    if let Some(cmd) = args.get("command") {
        let line = match cmd {
            // Codex passes argv as an array; Claude Code passes a string.
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        if !line.trim().is_empty() {
            ex.commands.push(Command {
                cmd: line,
                lang: None,
            });
        }
    }
    for key in ["path", "file_path", "filename"] {
        if let Some(p) = args.get(key).and_then(Value::as_str) {
            if !p.is_empty() {
                ex.files.push(FileRef {
                    path: p.to_string(),
                    tool: if name.is_empty() {
                        "codex".to_string()
                    } else {
                        name.clone()
                    },
                });
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> (Vec<ParsedExchange>, ParseReport) {
        Codex.parse(body, "/tmp/rollout-test.jsonl").unwrap()
    }

    /// Finding 2, the silent one: the same answer exists twice in the file, in
    /// two different streams, and both copies are real text.
    #[test]
    fn the_duplicate_event_stream_is_not_ingested() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev/api","git":{"branch":"main","repository_url":"https://example.invalid/r.git"}}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"how do I tail the log"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"event_msg","payload":{"type":"user_message","message":"how do I tail the log"}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Use tail -F so it survives a rotation."}]}}
{"timestamp":"2026-03-01T10:00:04.000Z","type":"event_msg","payload":{"type":"agent_message","message":"Use tail -F so it survives a rotation."}}
"#;
        let (ex, report) = parse(body);
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].response, "Use tail -F so it survives a rotation.");
        assert_eq!(
            ex[0].response.matches("tail -F").count(),
            1,
            "the event_msg copy was ingested too: {:?}",
            ex[0].response
        );
        assert_eq!(
            report.records_unknown_type, 0,
            "event_msg is known, not unknown"
        );
    }

    /// Finding 1: no record carries an identifier, so the key is the line.
    #[test]
    fn the_dedup_key_is_positional() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first question"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"first answer"}]}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"second question"}]}}
{"timestamp":"2026-03-01T10:00:04.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"second answer"}]}}
"#;
        let (ex, _) = parse(body);
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].source_key, "@2");
        assert_eq!(ex[1].source_key, "@4");
        // Two prompts with identical text would still be two rows — asking the
        // same question twice is legitimate history.
        assert_ne!(ex[0].source_key, ex[1].source_key);
    }

    /// Finding 3: the injected block shares the string with the prompt, so it
    /// is cut out rather than being grounds to reject the record. The Claude
    /// Code rule is the opposite and would lose the question entirely.
    #[test]
    fn environment_context_is_stripped_not_rejected() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\ncwd: /home/dev\nshell: zsh\n</environment_context>\nwhy is the build slow"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Incremental compilation is off."}]}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>sandbox</permissions instructions>"}]}}
"#;
        let (ex, _) = parse(body);
        assert_eq!(
            ex.len(),
            1,
            "the developer record must not start an exchange"
        );
        assert_eq!(ex[0].prompt, "why is the build slow");
        assert!(!ex[0].prompt.contains("shell: zsh"));
    }

    /// A record that is *only* an injected block starts nothing.
    #[test]
    fn a_record_that_is_all_injected_context_is_dropped() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd: /home/dev</environment_context>"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"orphan"}]}}
"#;
        let (ex, report) = parse(body);
        assert!(ex.is_empty());
        assert_eq!(report.orphaned_records, 1, "counted, never misattributed");
    }

    #[test]
    fn a_repository_url_becomes_the_bare_repo_name() {
        for (url, want) in [
            ("https://github.com/acme/billing-api.git", "billing-api"),
            ("https://github.com/acme/billing-api", "billing-api"),
            ("git@github.com:acme/billing-api.git", "billing-api"),
            ("https://example.invalid/r.git/", "r"),
        ] {
            assert_eq!(repo_name_from_url(url).as_deref(), Some(want), "{url}");
        }
        assert_eq!(repo_name_from_url(""), None);
    }

    /// Finding 5: `session_meta` hands over repo and branch outright, and every
    /// field inside `git` is individually optional.
    #[test]
    fn session_meta_supplies_metadata_and_turn_context_overrides_per_turn() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"sess-9","cwd":"/home/dev/a","git":{"repository_url":"https://example.invalid/r.git"}}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/home/dev/b","model":"gpt-5-codex"}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"where am i"}]}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"in b"}]}}
"#;
        let (ex, _) = parse(body);
        assert_eq!(ex[0].session_id, "sess-9");
        assert_eq!(
            ex[0].cwd, "/home/dev/b",
            "turn_context wins over session_meta"
        );
        assert_eq!(ex[0].model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(ex[0].git_branch, None, "no branch in this git object");
    }

    /// Commands are mined at capture or not at all — the raw block is never
    /// stored. Codex passes argv as an array where Claude Code passes a string.
    #[test]
    fn commands_and_files_are_mined_from_tool_calls() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run the tests"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"cargo\",\"test\",\"--all\"]}"}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","arguments":"{\"path\":\"/home/dev/src/main.rs\"}"}}
{"timestamp":"2026-03-01T10:00:04.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"All green."}]}}
"#;
        let (ex, _) = parse(body);
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].commands[0].cmd, "cargo test --all");
        assert_eq!(ex[0].files[0].path, "/home/dev/src/main.rs");
        assert_eq!(ex[0].files[0].tool, "apply_patch");
    }

    /// A prompt with no answer is not an exchange — the same rule as Claude
    /// Code, and what keeps an aborted turn out of the archive.
    #[test]
    fn a_prompt_with_no_response_is_not_written() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"abandoned question"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"event_msg","payload":{"type":"turn_aborted"}}
"#;
        let (ex, report) = parse(body);
        assert!(ex.is_empty());
        assert_eq!(report.prompts_found, 1);
        assert_eq!(report.prompts_without_response, 1);
    }

    /// Finding 6 and the records that appeared after the survey was written.
    /// An unrecognised type must be *reported*, and a recognised-but-ignored
    /// one must not be — a permanent false alarm teaches the user to ignore the
    /// line that says the format moved.
    #[test]
    fn known_noise_is_silent_and_genuinely_new_types_are_reported() {
        let body = r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"a question"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"an answer"}]}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"compacted","payload":{"message":"summary","replacement_history":[{"role":"user","content":"old"}]}}
{"type":"response.done","response":{"id":"r1","usage":{"total_tokens":9}}}
{"timestamp":"2026-03-01T10:00:05.000Z","type":"brand_new_thing","payload":{}}
"#;
        let (ex, report) = parse(body);
        assert_eq!(ex.len(), 1, "compaction did not re-ingest its history");
        assert_eq!(report.records_unknown_type, 1);
        assert_eq!(report.unknown_types, vec!["brand_new_thing"]);
    }

    /// Discovery is the trap the survey missed: `~/.codex` holds JSONL that is
    /// not a transcript, and ingesting it produces a permanent complaint about
    /// files that were never conversations.
    #[test]
    fn discovery_takes_only_rollout_files_under_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        std::fs::create_dir_all(root.join("2026/03/01")).unwrap();
        std::fs::write(root.join("2026/03/01/rollout-a.jsonl"), "").unwrap();
        std::fs::write(root.join("2026/03/01/notes.txt"), "").unwrap();
        // The two real decoys, both seen on a live machine.
        std::fs::write(dir.path().join("session_index.jsonl"), "").unwrap();
        std::fs::create_dir_all(dir.path().join(".tmp/plugins")).unwrap();
        std::fs::write(dir.path().join(".tmp/plugins/responses.jsonl"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("archived_sessions")).unwrap();
        std::fs::write(dir.path().join("archived_sessions/rollout-old.jsonl"), "").unwrap();

        let found = Codex.discover(&root).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].ends_with("rollout-a.jsonl"));
    }
}
