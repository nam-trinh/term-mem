//! The adapter for what `tmem run` records.
//!
//! This is the one transcript format term-mem writes itself, which makes it the
//! only one that cannot change without notice. It exists as an adapter anyway,
//! rather than as a direct database write, because that is what keeps the PTY
//! tier honest: redaction, the `forgotten` tombstone and idempotency are
//! properties of the ingest path, and a second way into the database would need
//! all three again.
//!
//! See `src/capture/pty.rs` for why the turn boundaries in this file are
//! trustworthy and the response text is not.

use super::{Adapter, DedupKey, ParseReport, ParsedExchange};
use crate::capture::pty::{Header, Turn};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct PtyRepl;

impl Adapter for PtyRepl {
    fn name(&self) -> &'static str {
        "pty"
    }

    fn transcript_root(&self) -> Result<PathBuf> {
        crate::paths::pty_sessions_dir()
    }

    fn discover(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        if !root.is_dir() {
            return Ok(out);
        }
        for e in std::fs::read_dir(root)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
        out.sort();
        Ok(out)
    }

    fn claims_path(&self, path: &Path) -> bool {
        // Our own directory, and nothing else — the format has no distinctive
        // filename, so the tree is the claim.
        crate::paths::pty_sessions_dir()
            .map(|d| path.starts_with(d))
            .unwrap_or(false)
    }

    fn injected_block_tags(&self) -> &'static [&'static str] {
        &[]
    }

    fn parse(&self, source: &str, path: &str) -> Result<(Vec<ParsedExchange>, ParseReport)> {
        let mut report = ParseReport::default();
        let mut out = Vec::new();
        let mut header: Option<Header> = None;

        for (i, line) in source.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            report.records_total += 1;
            if header.is_none() {
                match serde_json::from_str::<Header>(line) {
                    Ok(h) if h.kind == "tmem-pty" => {
                        header = Some(h);
                        continue;
                    }
                    // A file in our directory that is not one of ours. Loud,
                    // because the alternative is parsing an unknown format into
                    // a plausible-looking archive.
                    _ => {
                        anyhow::bail!(
                            "{path}: not a tmem pty recording (no `tmem-pty` header on line 1)"
                        );
                    }
                }
            }
            let Some(h) = &header else { unreachable!() };
            let turn: Turn = match serde_json::from_str(line) {
                Ok(t) => t,
                Err(_) => {
                    report.records_unparsable += 1;
                    continue;
                }
            };
            report.prompts_found += 1;
            if turn.response.trim().is_empty() {
                report.prompts_without_response += 1;
                continue;
            }
            out.push(ParsedExchange {
                session_id: h.session_id.clone(),
                // One recording is one conversation: a REPL has no `/clear`
                // that starts a second tree in the same file.
                thread_id: h.session_id.clone(),
                // Positional, for the same reason as Codex: the format carries
                // no per-turn identity, and the file is append-only because we
                // are the ones appending to it.
                source_key: DedupKey::Positional { line: i + 1 }.as_str(),
                ts_ms: turn.ts_ms,
                cwd: h.cwd.clone(),
                repo: None,
                git_branch: None,
                // The REPL, not the model. `ollama run llama3` is recorded as
                // `ollama llama3`, because which model answered is the argv and
                // nothing in the stream says otherwise.
                model: Some(h.argv.join(" ")),
                prompt: turn.prompt,
                response: turn.response,
                commands: Vec::new(),
                files: Vec::new(),
            });
        }
        Ok((out, report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"kind":"tmem-pty","repl":"ollama","argv":["ollama","run","llama3"],"cwd":"/home/dev/src/api","session_id":"01SESSION","started_ms":1772000000000}"#;

    #[test]
    fn a_recording_parses_into_exchanges() {
        let body = format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"ts_ms":1772000000000,"prompt":"what is a pty","response":"A pair of devices."}"#,
            r#"{"ts_ms":1772000001000,"prompt":"and a tty","response":"The terminal side of one."}"#
        );
        let (ex, report) = PtyRepl.parse(&body, "/tmp/pty/01.jsonl").unwrap();
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].prompt, "what is a pty");
        assert_eq!(ex[0].session_id, "01SESSION");
        assert_eq!(ex[0].model.as_deref(), Some("ollama run llama3"));
        assert_eq!(ex[0].cwd, "/home/dev/src/api");
        // Positional keys, distinct per turn.
        assert_eq!(ex[0].source_key, "@2");
        assert_eq!(ex[1].source_key, "@3");
        assert_eq!(report.prompts_found, 2);
    }

    /// A file in our own directory that is not ours must be an error, not a
    /// best-effort parse into a plausible archive.
    #[test]
    fn a_file_without_our_header_is_refused() {
        let e = PtyRepl
            .parse("{\"something\":\"else\"}\n", "/tmp/pty/x.jsonl")
            .unwrap_err();
        assert!(
            format!("{e:#}").contains("not a tmem pty recording"),
            "{e:#}"
        );
    }

    #[test]
    fn a_turn_with_no_answer_is_counted_and_dropped() {
        let body = format!(
            "{HEADER}\n{}\n",
            r#"{"ts_ms":1772000000000,"prompt":"interrupted","response":"   "}"#
        );
        let (ex, report) = PtyRepl.parse(&body, "/tmp/pty/01.jsonl").unwrap();
        assert!(ex.is_empty());
        assert_eq!(report.prompts_without_response, 1);
    }
}
