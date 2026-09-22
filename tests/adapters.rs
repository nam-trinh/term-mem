//! Phase 6 — widening capture past one assistant.
//!
//! docs/plan.md's Exit for this phase is implicit in its scope: the parser
//! interface generalizes, subagent transcripts stop being invisible, and Codex
//! CLI ingests without the two silent traps its survey found. Each of those is
//! driven here against the real binary and checked-in fixtures.
//!
//! The traps under test are the silent ones. A parser that double-counts every
//! Codex response produces a plausible archive that is wrong, and a discovery
//! walk one level too shallow produced four phases of "zero sidechain records"
//! from an archive that had 445 of them.

mod common;

use common::Env;
use predicates::prelude::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Install a fixture into an adapter-appropriate place in the fake tree.
fn install_at(e: &Env, fixture: &str, rel: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let dst = e.home().join(rel);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
    dst
}

// ── Subagent transcripts ─────────────────────────────────────────────────

/// Phase 2 finding 3: `<project>/<session>/subagents/agent-*.jsonl` is a
/// directory nothing looked in, which is why Phases 0 and 1 both recorded
/// "zero sidechain records". Discovery walked one level; the tree has two.
#[test]
fn subagent_transcripts_are_discovered_and_ingested() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    install_at(
        &e,
        "claude_code/subagent-transcript.jsonl",
        "projects/proj/sess-1/subagents/agent-abc.jsonl",
    );

    e.cmd().args(["capture", "--all"]).assert().success();

    assert_eq!(
        e.count("exchanges"),
        1,
        "one agent invocation, one exchange"
    );
    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    assert!(
        prompt.contains("Review the diff"),
        "the isMeta root is the prompt: {prompt}"
    );
    let response = e.query("SELECT response FROM exchanges").remove(0);
    assert!(response.contains("retry loop"), "{response}");
}

/// The same flag means opposite things in the two kinds of transcript, so the
/// rules that make a main transcript parse correctly must not be relaxed for
/// it by the ones that make a subagent transcript parse at all.
#[test]
fn a_main_transcript_still_folds_its_inline_sidechain_turns() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.ingest("sidechain.jsonl");
    // Unchanged from Phase 1: an inline sidechain turn belongs to the exchange
    // above it and never starts one of its own.
    assert_eq!(e.count("exchanges"), 1);
}

/// A subagent runs inside its parent's session, so its rows carry the parent's
/// `session_id` — but `--session` groups on `thread_id`, and the agent's tree
/// has its own root. Same session, different thread.
#[test]
fn a_subagent_thread_does_not_merge_into_its_parent_conversation() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.ingest("finding-09-many-to-one.jsonl");
    install_at(
        &e,
        "claude_code/subagent-transcript.jsonl",
        "projects/proj/sess-1/subagents/agent-abc.jsonl",
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    let threads = e.query("SELECT DISTINCT thread_id FROM exchanges");
    assert!(
        threads.len() >= 2,
        "the subagent shares a thread with its parent: {threads:?}"
    );
}

// ── Codex CLI ────────────────────────────────────────────────────────────

fn codex_env(e: &Env) -> assert_cmd::Command {
    let mut c = e.cmd();
    c.env("TMEM_CODEX_SESSIONS", e.home().join("codex/sessions"));
    c
}

/// The whole Codex pipeline, from a fixture holding every record shape the real
/// archive contains.
#[test]
fn codex_transcripts_ingest_with_metadata_and_mined_commands() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    install_at(
        &e,
        "codex/all-record-shapes.jsonl",
        "codex/sessions/2026/03/01/rollout-2026-03-01T10-00-00-abc.jsonl",
    );

    codex_env(&e).args(["capture", "--all"]).assert().success();

    assert_eq!(e.count("exchanges"), 1);
    let row = e
        .query("SELECT assistant || '|' || repo || '|' || git_branch || '|' || cwd FROM exchanges")
        .remove(0);
    // Finding 5: Codex hands over repo and branch outright, which Claude Code
    // never provides.
    assert_eq!(row, "codex-cli|repo|main|/home/dev/src/api", "{row}");

    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    assert!(prompt.contains("migration hold a lock"), "{prompt}");

    // Finding 2, the silent one: the answer exists twice in the file, in two
    // streams. Exactly one copy may reach the archive.
    let response = e.query("SELECT response FROM exchanges").remove(0);
    assert_eq!(
        response.matches("Batch it with a checkpoint table").count(),
        1,
        "the event_msg duplicate was ingested too: {response}"
    );

    // Mined at capture or not at all — the raw block is never stored.
    let cmds = e.query("SELECT cmd FROM commands");
    assert!(cmds.iter().any(|c| c.contains("cargo test")), "{cmds:?}");
}

/// Finding 1: no record carries an identifier, so the key is `@<line>` — and
/// idempotency has to hold on it just as firmly as on Claude Code's uuid.
#[test]
fn re_ingesting_a_codex_transcript_is_a_no_op() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    install_at(
        &e,
        "codex/all-record-shapes.jsonl",
        "codex/sessions/2026/03/01/rollout-a.jsonl",
    );

    codex_env(&e).args(["capture", "--all"]).assert().success();
    let first = e.rows();
    assert_eq!(first.len(), 1);

    for _ in 0..3 {
        codex_env(&e)
            .args(["capture", "--all", "--quiet"])
            .assert()
            .success();
    }
    assert_eq!(e.rows(), first, "re-ingest was not a no-op");
    assert_eq!(e.count("commands"), 1, "derived rows duplicated");

    let key = e.query("SELECT source_key FROM exchanges").remove(0);
    assert!(key.starts_with('@'), "positional key expected: {key}");
}

/// Discovery is the trap the survey missed. `~/.codex` holds JSONL that is not
/// a transcript, and every one of them would be reported as a file term-mem
/// could not understand — a permanent complaint about files that were never
/// conversations.
#[test]
fn codex_discovery_ignores_the_non_transcript_jsonl_in_the_same_tree() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    install_at(
        &e,
        "codex/all-record-shapes.jsonl",
        "codex/sessions/2026/03/01/rollout-a.jsonl",
    );
    // Both decoys are real, and both were on the machine this was written on.
    std::fs::write(
        e.home().join("codex/sessions/session_index.jsonl"),
        "{\"id\":\"x\",\"thread_name\":\"Build an app\",\"updated_at\":\"2026-03-24T00:09:02Z\"}\n",
    )
    .unwrap();
    std::fs::create_dir_all(e.home().join("codex/sessions/.tmp/plugins")).unwrap();
    std::fs::write(
        e.home().join("codex/sessions/.tmp/plugins/responses.jsonl"),
        "{\"type\":\"response.done\",\"response\":{\"id\":\"r1\"}}\n",
    )
    .unwrap();

    let out = codex_env(&e).args(["capture", "--all"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("from 1 transcript(s)"), "{stdout}");
    assert!(
        !stdout.contains("unrecognised record type"),
        "a non-transcript was parsed: {stdout}"
    );
    assert_eq!(e.count("exchanges"), 1);
}

/// `--path` has no tree around it to say whose file it is, so the filename
/// decides — and `--assistant` overrides when it cannot.
#[test]
fn a_single_file_is_routed_to_the_right_adapter() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let rollout = install_at(
        &e,
        "codex/all-record-shapes.jsonl",
        "elsewhere/rollout-2026-03-01T10-00-00-abc.jsonl",
    );
    e.cmd()
        .args(["capture", "--path"])
        .arg(&rollout)
        .assert()
        .success();
    assert_eq!(
        e.query("SELECT assistant FROM exchanges").remove(0),
        "codex-cli",
        "the rollout- prefix is a Codex name"
    );

    // And an explicit override is honoured, including its error case.
    e.cmd()
        .args(["capture", "--path"])
        .arg(&rollout)
        .args(["--assistant", "no-such-assistant"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("known: claude-code, codex-cli"));
}

/// Both vendors in one archive, searchable together — which is the point of
/// the phase, and the thing a per-vendor tool could not do.
#[test]
fn both_assistants_land_in_one_searchable_archive() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    e.write_transcript(
        "claude.jsonl",
        &format!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s1","timestamp":"2026-03-02T10:00:00.000Z","cwd":"/home/dev/src/api","gitBranch":"main","message":{{"role":"user","content":{}}}}}
{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-03-02T10:00:01.000Z","cwd":"/home/dev/src/api","gitBranch":"main","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":{}}}]}}}}
"#,
            serde_json::to_string("how do I lock a postgres table safely").unwrap(),
            serde_json::to_string("Use an explicit LOCK TABLE in the smallest transaction you can.").unwrap(),
        ),
    );
    install_at(
        &e,
        "codex/all-record-shapes.jsonl",
        "codex/sessions/2026/03/01/rollout-a.jsonl",
    );

    codex_env(&e).args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 2);

    // One query, both vendors.
    let out = codex_env(&e)
        .args(["search", "lock", "--json"])
        .output()
        .unwrap();
    let assistants: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["assistant"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(
        assistants.contains(&"claude-code".to_string()),
        "{assistants:?}"
    );
    assert!(
        assistants.contains(&"codex-cli".to_string()),
        "{assistants:?}"
    );

    // And `doctor` reports each tree separately, because a machine with one
    // assistant installed and not the other is the ordinary case.
    codex_env(&e)
        .args(["doctor"])
        .assert()
        .stdout(predicate::str::contains("codex-cli transcript(s)"))
        .stdout(predicate::str::contains("claude-code transcript(s)"));
}

/// An assistant with no transcripts on the machine is a note, not a problem.
/// Reporting it as one is how a health check trains people to stop reading it.
#[test]
fn a_missing_assistant_tree_is_not_a_failure() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.ingest("finding-09-many-to-one.jsonl");
    // `doctor` exits 2 here for unrelated reasons (no Stop hook in a temp
    // home), which is the point: a missing second assistant must not be one of
    // them, and must appear as a note rather than a `!!`.
    let out = e
        .cmd()
        .env("TMEM_CODEX_SESSIONS", e.home().join("no-codex-here"))
        .args(["doctor"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("--  no codex-cli transcripts"),
        "reported as a problem rather than a note: {stdout}"
    );
    assert!(stdout.contains("claude-code transcript(s)"), "{stdout}");
}

/// Deletion reaches a Codex row the same way it reaches a Claude Code one.
/// Every write path redacts and honours the tombstone; a second adapter is a
/// second write path.
#[test]
fn forget_and_redaction_apply_to_the_second_adapter_too() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    let path = e
        .home()
        .join("codex/sessions/2026/03/01/rollout-secret.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"the deploy fails with Authorization: Bearer sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"That token is expired; rotate it."}]}}
"#,
    )
    .unwrap();

    codex_env(&e).args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 1);
    // Redaction runs on this write path too.
    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    assert!(!prompt.contains("sk-ant-api03-AAAA"), "{prompt}");
    assert_eq!(
        e.query("SELECT CAST(redacted AS TEXT) FROM exchanges")
            .remove(0),
        "1"
    );

    // And the tombstone holds against a re-ingest of the same file.
    let id = e.query("SELECT id FROM exchanges").remove(0);
    codex_env(&e).args(["forget", &id, "-y"]).assert().success();
    codex_env(&e)
        .args(["capture", "--all", "--quiet"])
        .assert()
        .success();
    assert_eq!(e.count("exchanges"), 0, "forget was undone by a re-ingest");
}

// ── The PTY tier ─────────────────────────────────────────────────────────

/// The allowlist *is* the policy. docs/plan.md: "we never watch the terminal.
/// Capture happens only from processes with an explicit adapter." `tmem run`
/// must not become the flag that undoes that.
#[test]
fn run_refuses_anything_without_an_adapter() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    for forbidden in ["bash", "zsh", "vim", "ssh", "psql", "cat"] {
        e.cmd()
            .args(["run", forbidden])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("never watches the terminal"));
    }
    // With no argument it explains itself rather than doing anything.
    e.cmd()
        .args(["run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ollama"))
        .stdout(predicate::str::contains("lossy"));
}

/// A recording goes in through the ordinary ingest door, so it picks up
/// redaction and the `forgotten` tombstone without a second implementation of
/// either. Driven through the adapter rather than a live pty, because what is
/// under test here is the write path.
#[test]
fn a_pty_recording_is_ingested_redacted_and_forgettable() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let dir = e.data().join("pty");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("01SESSION.jsonl"),
        format!(
            "{}\n{}\n",
            r#"{"kind":"tmem-pty","repl":"ollama","argv":["ollama","run","llama3"],"cwd":"/home/dev/src/api","session_id":"01SESSION","started_ms":1772000000000}"#,
            r#"{"ts_ms":1772000000000,"prompt":"why is my deploy failing with ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","response":"That token is expired. Rotate it and retry."}"#
        ),
    )
    .unwrap();

    e.cmd().args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 1);
    assert_eq!(e.query("SELECT assistant FROM exchanges").remove(0), "pty");

    // Redaction runs on this path like every other.
    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    assert!(!prompt.contains("ghp_AAAA"), "{prompt}");
    assert!(prompt.contains("[redacted:"), "{prompt}");

    // And the tombstone holds against a re-ingest.
    let id = e.query("SELECT id FROM exchanges").remove(0);
    e.cmd().args(["forget", &id, "-y"]).assert().success();
    e.cmd()
        .args(["capture", "--all", "--quiet"])
        .assert()
        .success();
    assert_eq!(e.count("exchanges"), 0);
}

/// A file in term-mem's own pty directory that term-mem did not write is an
/// error, not a best-effort parse. Silent failure is the enemy, and this is
/// the one format we control, so a surprise here means something else is
/// writing there.
#[test]
fn a_foreign_file_in_the_pty_directory_is_refused_loudly() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let dir = e.data().join("pty");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stray.jsonl"), "{\"not\":\"ours\"}\n").unwrap();

    e.cmd()
        .args(["capture", "--all"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a tmem pty recording"));
}

/// End to end through a real pty, with a real child process.
///
/// `sgpt` stands in for a REPL here only in the sense that it is on the
/// allowlist; what actually runs is a shell script pretending to be one, so the
/// test does not need a model. What is under test is the part that is hard: raw
/// mode, the echo, the ANSI stripping, and the Enter that ends a turn.
///
/// **Input is typed, not piped.** The first version wrote both lines at once
/// and was flaky — it passed five CI runs and failed the sixth, because whether
/// the quiescence timer had promoted and closed the turn before the child
/// exited was a race. That race is real and documented (the PTY tier merges
/// turns a REPL answers faster than the quiet window), but it is the *lossy*
/// path, and asserting on it asserts on a coin flip. A flaky test is worse than
/// no test: it teaches whoever sees it to press re-run, which is the lesson
/// phase-4.md finding 10 already recorded about a different suite here.
///
/// So this writes one line, waits past the 300 ms quiet window, then writes the
/// next — which is what a person at a REPL does, and is the case the feature
/// actually supports.
#[test]
#[cfg_attr(not(unix), ignore)]
fn a_live_pty_session_records_the_turns_a_user_typed() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let bin = fake_repl(&e);

    let mut child = Command::new(assert_cmd::cargo::cargo_bin("tmem"))
        .args(["run", "sgpt"])
        .env("TMEM_HOME", e.home().join("data"))
        .env("TMEM_CLAUDE_PROJECTS", e.projects())
        .env("TMEM_CLAUDE_SETTINGS", e.settings())
        .env("TMEM_CONFIG_DIR", e.home().join("no-config"))
        .env("HOME", e.home())
        .env("PATH", with_path(&bin))
        .env_remove("TMEM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        // Let the REPL print its banner before asking anything.
        std::thread::sleep(std::time::Duration::from_millis(200));
        stdin.write_all(b"joining mp4 files\n").unwrap();
        stdin.flush().unwrap();
        // Past the quiet window, so the turn is unambiguously finished.
        std::thread::sleep(std::time::Duration::from_millis(700));
        stdin.write_all(b"bye\n").unwrap();
        stdin.flush().unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");

    assert_eq!(e.count("exchanges"), 1, "stderr: {stderr}");
    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    let response = e.query("SELECT response FROM exchanges").remove(0);
    assert_eq!(prompt, "joining mp4 files");
    assert!(response.contains("concat demuxer"), "{response:?}");
    // The escape codes the fake REPL emitted must not be in the archive.
    assert!(!response.contains('\u{1b}'), "{response:?}");
    // Nor the echo of the question — the fake REPL deliberately does not quote
    // it back, so any occurrence here is the pty's echo rather than content.
    assert!(
        !response.contains("joining mp4 files"),
        "echo leaked: {response:?}"
    );
    assert!(
        !response.contains("bye"),
        "the queued line leaked: {response:?}"
    );
}

/// The lossy path, asserted on what it actually promises rather than on a
/// race: piping a script of questions merges turns, and the command says so.
#[test]
#[cfg_attr(not(unix), ignore)]
fn piped_input_may_merge_turns_and_always_reports_the_gap() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let bin = fake_repl(&e);

    let out = e
        .cmd()
        .env("PATH", with_path(&bin))
        .args(["run", "sgpt"])
        .write_stdin("first question\nsecond question\nbye\n")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // However many turns survive — and it is genuinely timing-dependent — the
    // count of what was sent and what was kept has to add up in the report.
    let kept = e.count("exchanges");
    assert!(kept <= 2, "more turns than questions: {kept}");
    if kept < 2 {
        assert!(
            stderr.contains("produced no separate exchange"),
            "turns were dropped without saying so: {stderr}"
        );
    }
}

// ── Regressions from the Phase 6 review, and from using the thing ────────

/// docs/cli.md: "one who believes it's paused when it's recording gets a nasty
/// surprise." `tmem run` printed exactly that reassurance and then recorded —
/// the session file, the database row, all of it.
#[test]
fn run_records_nothing_at_all_while_capture_is_paused() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let bin = fake_repl(&e);
    e.cmd().args(["pause"]).assert().success();

    let out = e
        .cmd()
        .env("PATH", with_path(&bin))
        .args(["run", "sgpt"])
        .write_stdin("how do I join mp4 files\nbye\n")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("without recording"),
        "it must say so"
    );

    assert_eq!(e.count("exchanges"), 0, "recorded while paused");
    // And nothing on disk either — a session file deleted afterwards would
    // still have existed.
    let pty_dir = e.data().join("pty");
    let left = std::fs::read_dir(&pty_dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "a recording was written while paused");

    // Resumed, it records again.
    e.cmd().args(["resume"]).assert().success();
    e.cmd()
        .env("PATH", with_path(&bin))
        .args(["run", "sgpt"])
        .write_stdin("how do I join mp4 files\nbye\n")
        .output()
        .unwrap();
    assert_eq!(e.count("exchanges"), 1);
}

/// `line.push(b as char)` stored `concaténer` as `concatÃ©ner`, and the mangled
/// prompt then failed to match the correctly-decoded echo, so the answer was
/// filed under the wrong question.
#[test]
fn a_non_ascii_question_survives_a_live_pty_session() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let bin = fake_repl(&e);

    e.cmd()
        .env("PATH", with_path(&bin))
        .args(["run", "sgpt"])
        .write_stdin("comment concaténer les fichiers mp4 ?\nbye\n")
        .output()
        .unwrap();

    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    assert_eq!(prompt, "comment concaténer les fichiers mp4 ?");
    assert!(
        !prompt.contains('Ã'),
        "mojibake reached the archive: {prompt}"
    );

    // And the echo was still stripped, which the mangling used to prevent.
    let response = e.query("SELECT response FROM exchanges").remove(0);
    assert!(!response.contains("concaténer"), "echo leaked: {response}");
    assert!(response.contains("concat demuxer"), "{response}");
}

/// Found by reading a real archive, not by reading the diff: 16 of 84 Codex
/// prompts carried an IDE context block, the largest 6,387 characters of it.
/// The stripper knew about angle brackets; this wrapper is markdown.
#[test]
fn an_ide_context_block_never_reaches_the_archive() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    let path = e.home().join("codex/sessions/2026/03/01/rollout-ide.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let wrapped = "# Context from my IDE setup:\n\n## Active file: README.md\n\n\
                   ## Open tabs:\n- README.md: README.md\n\n\
                   ## My request for Codex:\nwhy does the migration lock the table";
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n{}\n",
            r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}"#,
            serde_json::json!({
                "timestamp": "2026-03-01T10:00:01.000Z", "type": "response_item",
                "payload": {"type":"message","role":"user",
                            "content":[{"type":"input_text","text":wrapped}]}
            }),
            r#"{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"It rewrites every row in one transaction."}]}}"#
        ),
    )
    .unwrap();

    codex_env(&e).args(["capture", "--all"]).assert().success();
    let prompt = e.query("SELECT prompt FROM exchanges").remove(0);
    assert_eq!(prompt, "why does the migration lock the table");
    assert!(!prompt.contains("Open tabs"), "{prompt}");
    assert!(!prompt.contains("Active file"), "{prompt}");
}

/// A skipped Codex user record left `current` pointing at the previous
/// exchange, so the next answer was appended to it — silently, and uncounted.
#[test]
fn a_reply_to_a_skipped_codex_question_does_not_land_on_the_previous_row() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    let path = e
        .home()
        .join("codex/sessions/2026/03/01/rollout-skip.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"question ONE"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer ONE"}]}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<turn_aborted>only injected</turn_aborted>"}]}}
{"timestamp":"2026-03-01T10:00:04.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer TWO belongs elsewhere"}]}}
"#,
    )
    .unwrap();

    codex_env(&e).args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 1);
    let response = e.query("SELECT response FROM exchanges").remove(0);
    assert_eq!(response, "answer ONE", "{response}");
}

/// A `payload.type` nobody recognises must reach the user. Without it, a rename
/// of `function_call` stops command mining permanently and `doctor` says
/// nothing.
#[test]
fn an_unknown_codex_payload_type_is_reported_to_the_user() {
    let e = Env::new();
    codex_env(&e).args(["init", "--no-hook"]).assert().success();
    let path = e.home().join("codex/sessions/2026/03/01/rollout-new.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"timestamp":"2026-03-01T10:00:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/dev"}}
{"timestamp":"2026-03-01T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run the tests"}]}}
{"timestamp":"2026-03-01T10:00:02.000Z","type":"response_item","payload":{"type":"shell_call_v2","name":"shell"}}
{"timestamp":"2026-03-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"All green."}]}}
"#,
    )
    .unwrap();

    codex_env(&e)
        .args(["capture", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("response_item/shell_call_v2"));
}

/// The fake REPL used by the pty tests, and the PATH that finds it.
fn fake_repl(e: &Env) -> PathBuf {
    let dir = e.home().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("sgpt");
    std::fs::write(
        &f,
        "#!/bin/sh\nprintf '>>> '\nwhile IFS= read -r line; do\n  \
         [ \"$line\" = bye ] && exit 0\n  \
         printf 'Answer: use the concat demuxer with -c copy.\\n>>> '\n\
         done\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn with_path(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}
