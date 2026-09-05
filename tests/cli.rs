//! Integration tests, written against the Phase 1 **Exit** criterion in
//! docs/plan.md:
//!
//! > the author runs it against their own daily work for two weeks without
//! > losing an exchange, duplicating one, or noticing it running.
//!
//! Two weeks of wall-clock cannot be tested. The three failure modes it names
//! can: *losing* one, *duplicating* one, and the latency that would make it
//! noticeable (that last one is in `budget.rs`).

mod common;

use common::Env;
use predicates::prelude::*;

// ── not losing an exchange ───────────────────────────────────────────────

#[test]
fn every_answered_prompt_in_a_transcript_reaches_the_archive() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    e.ingest("finding-05-two-threads.jsonl");
    // one from the many-to-one fixture, three from the two-thread fixture
    assert_eq!(e.count("exchanges"), 4);
    e.cmd().arg("recent").assert().success();
}

#[test]
fn nothing_injected_is_stored_as_a_prompt() {
    let e = Env::new();
    e.ingest("finding-02-user-records.jsonl");
    let prompts = e.query("SELECT prompt FROM exchanges");
    assert_eq!(prompts.len(), 2);
    for p in &prompts {
        assert!(
            !p.contains("ide_opened_file"),
            "editor telemetry in the archive: {p}"
        );
        assert!(
            !p.contains("command-name"),
            "slash-command echo in the archive: {p}"
        );
        assert!(!p.contains("continued from a previous conversation"));
    }
}

#[test]
fn commands_and_file_refs_are_mined_at_capture_time() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let cmds = e.query("SELECT cmd FROM commands ORDER BY seq");
    assert!(cmds.iter().any(|c| c.starts_with("ffmpeg -f concat")));
    assert!(cmds.iter().any(|c| c.starts_with("ffprobe")));
    assert_eq!(e.count("file_refs"), 2);
    // The Edit payload itself is never stored — only the path it touched.
    let resp = e.query("SELECT response FROM exchanges").pop().unwrap();
    assert!(!resp.contains("old_string"));
}

// ── not duplicating one ─────────────────────────────────────────────────

#[test]
fn re_ingesting_the_same_transcript_is_a_no_op() {
    let e = Env::new();
    let p = e.ingest("finding-09-many-to-one.jsonl");
    let before = e.rows();
    assert_eq!(before.len(), 1);

    for _ in 0..3 {
        e.cmd()
            .args(["capture", "--path"])
            .arg(&p)
            .assert()
            .success();
    }
    // Assert the ids, not merely the absence of an error: a second row with a
    // new ULID would still exit zero.
    assert_eq!(e.rows(), before);
    assert_eq!(e.count("commands"), 2);
    assert_eq!(e.count("file_refs"), 2);
}

#[test]
fn a_growing_transcript_completes_its_row_rather_than_adding_one() {
    let e = Env::new();
    let p = e.install("finding-09-many-to-one.jsonl");
    // Ingest a truncated prefix first, as a mid-turn hook would see it.
    let full = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = full.lines().collect();
    std::fs::write(&p, lines[..3].join("\n") + "\n").unwrap();
    e.cmd()
        .args(["capture", "--path"])
        .arg(&p)
        .assert()
        .success();
    let first = e.rows();
    assert_eq!(first.len(), 1);
    let partial = e.query("SELECT response FROM exchanges").pop().unwrap();

    std::fs::write(&p, &full).unwrap();
    e.cmd()
        .args(["capture", "--path"])
        .arg(&p)
        .assert()
        .success();
    assert_eq!(e.rows(), first, "the same exchange, not a second one");
    let complete = e.query("SELECT response FROM exchanges").pop().unwrap();
    assert!(complete.len() > partial.len(), "the row grew with the turn");
    assert!(complete.contains("holds all four clips"));
}

#[test]
fn a_retried_prompt_after_an_api_error_produces_one_row() {
    let e = Env::new();
    e.ingest("finding-06-api-error-retry.jsonl");
    assert_eq!(e.count("exchanges"), 1);
    let r = e.query("SELECT response FROM exchanges").pop().unwrap();
    assert!(
        !r.contains("OAuth"),
        "an API error must never be stored as a response"
    );
}

#[test]
fn an_unchanged_file_is_not_reparsed() {
    let e = Env::new();
    let p = e.ingest("finding-09-many-to-one.jsonl");
    // --all honours the watermark; --path forces.
    e.cmd()
        .args(["capture", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 unchanged"));
    let _ = p;
}

// ── browsing ────────────────────────────────────────────────────────────

#[test]
fn show_session_groups_on_the_thread_not_the_file() {
    let e = Env::new();
    e.ingest("finding-05-two-threads.jsonl");
    let ids = e.query("SELECT id FROM exchanges ORDER BY ts");
    // Three exchanges in one file under one session id, but two conversations.
    let out = e
        .cmd()
        .args(["show", &ids[0], "--session", "--json"])
        .assert()
        .success();
    let lines = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        lines.trim().lines().count(),
        2,
        "/clear must not merge threads"
    );
}

#[test]
fn show_session_crosses_a_compaction_boundary() {
    let e = Env::new();
    e.ingest("finding-04-compaction.jsonl");
    let ids = e.query("SELECT id FROM exchanges ORDER BY ts");
    let out = e
        .cmd()
        .args(["show", &ids[0], "--session", "--json"])
        .assert()
        .success();
    let lines = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        lines.trim().lines().count(),
        2,
        "a compaction must not truncate the thread"
    );
}

#[test]
fn log_in_filters_by_directory_tree_without_prefix_bleed() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    e.cmd()
        .args(["log", "--in", "/home/dev/talks/pycon-2026"])
        .assert()
        .success();
    // A sibling with a shared prefix must not match.
    e.cmd()
        .args(["log", "--in", "/home/dev/talks/pycon"])
        .assert()
        .code(1);
}

#[test]
fn ids_resolve_from_a_unique_prefix() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let id = e.rows().pop().unwrap();
    e.cmd().args(["show", &id[..8]]).assert().success();
}

#[test]
fn exit_codes_carry_meaning() {
    let e = Env::new();
    // 1 — nothing found
    e.cmd().arg("recent").assert().code(1);
    e.ingest("finding-09-many-to-one.jsonl");
    // 0 — found
    e.cmd().arg("recent").assert().code(0);
    // 1 — no such id
    e.cmd()
        .args(["show", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"])
        .assert()
        .code(1);
    // 2 — error
    e.cmd()
        .args(["recent", "--since", "last tuesday-ish"])
        .assert()
        .code(2);
}

#[test]
fn json_is_one_record_per_line() {
    let e = Env::new();
    e.ingest("finding-05-two-threads.jsonl");
    let out = e.cmd().args(["recent", "--json"]).assert().success();
    let s = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(s.trim().lines().count(), 3);
    for l in s.trim().lines() {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        assert!(v.get("id").is_some() && v.get("prompt").is_some());
    }
}

// ── capture control ─────────────────────────────────────────────────────

#[test]
fn pause_stops_the_hook_and_status_says_so() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let p = e.install("finding-09-many-to-one.jsonl");
    e.cmd().arg("pause").assert().success();
    e.cmd()
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("PAUSED"));

    let payload = format!(
        r#"{{"session_id":"s","cwd":"/home/dev","transcript_path":"{}"}}"#,
        p.display()
    );
    e.cmd()
        .args(["capture", "--hook", "claude-code"])
        .env("TMEM_NO_SPAWN", "1")
        .write_stdin(payload.clone())
        .assert()
        .success();
    e.cmd().args(["capture", "--drain"]).assert().success();
    assert_eq!(e.count("exchanges"), 0, "paused means paused");

    e.cmd().arg("resume").assert().success();
    e.cmd()
        .args(["capture", "--hook", "claude-code"])
        .env("TMEM_NO_SPAWN", "1")
        .write_stdin(payload)
        .assert()
        .success();
    e.cmd().args(["capture", "--drain"]).assert().success();
    assert_eq!(e.count("exchanges"), 1);
}

#[test]
fn tmem_zero_disables_this_invocation_only() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let p = e.install("finding-09-many-to-one.jsonl");
    let payload = format!(
        r#"{{"session_id":"s","cwd":"/home/dev","transcript_path":"{}"}}"#,
        p.display()
    );
    e.cmd()
        .args(["capture", "--hook", "claude-code"])
        .env("TMEM", "0")
        .env("TMEM_NO_SPAWN", "1")
        .write_stdin(payload)
        .assert()
        .success();
    e.cmd().args(["capture", "--drain"]).assert().success();
    assert_eq!(e.count("exchanges"), 0);
}

#[test]
fn ignored_paths_are_never_recorded() {
    let e = Env::new();
    e.cmd()
        .args(["ignore", "/home/dev/talks"])
        .assert()
        .success();
    e.cmd()
        .args(["ignore", "--list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/home/dev/talks"));
    e.ingest("finding-09-many-to-one.jsonl");
    assert_eq!(e.count("exchanges"), 0, "cwd is under an ignored tree");

    e.cmd()
        .args(["ignore", "--remove", "/home/dev/talks"])
        .assert()
        .success();
    e.cmd()
        .args(["capture", "--path"])
        .arg(e.projects().join("proj/finding-09-many-to-one.jsonl"))
        .assert()
        .success();
    assert_eq!(e.count("exchanges"), 1);
}

// ── the safety valve ────────────────────────────────────────────────────

#[test]
fn forget_last_removes_the_row_and_everything_derived_from_it() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    assert_eq!(e.count("exchanges"), 1);
    assert!(e.count("commands") > 0);
    e.cmd().args(["forget", "--last", "-y"]).assert().success();
    assert_eq!(e.count("exchanges"), 0);
    assert_eq!(e.count("commands"), 0, "derived command rows must go too");
    assert_eq!(e.count("file_refs"), 0);
}

/// docs/mission.md: "Deleting a memory means it's gone." Not a flag on a row
/// that stays greppable — so this greps the raw database file on disk.
#[test]
fn a_forgotten_exchange_is_not_recoverable_from_the_database_file() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let needle = b"holds all four clips";
    let before = std::fs::read(e.db()).unwrap();
    assert!(
        before.windows(needle.len()).any(|w| w == needle),
        "precondition: the text is in the file before the delete"
    );

    let id = e.rows().pop().unwrap();
    e.cmd().args(["forget", &id, "-y"]).assert().success();

    for f in ["memory.db", "memory.db-wal"] {
        let p = e.home().join("data").join(f);
        if let Ok(bytes) = std::fs::read(&p) {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "the forgotten text is still recoverable from {f}"
            );
        }
    }
}

#[test]
fn forget_on_an_empty_archive_says_so_rather_than_failing() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.cmd().args(["forget", "--last", "-y"]).assert().code(1);
}

// ── setup ───────────────────────────────────────────────────────────────

#[test]
fn init_registers_the_stop_hook_without_clobbering_settings() {
    let e = Env::new();
    std::fs::write(
        e.settings(),
        r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
    )
    .unwrap();
    e.cmd().arg("init").assert().success();

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.settings()).unwrap()).unwrap();
    assert_eq!(v["model"], "opus", "unrelated settings must survive");
    let stop = v["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2, "the existing hook must survive");
    let all = serde_json::to_string(&v).unwrap();
    assert!(all.contains("tmem capture --hook claude-code"));
    assert!(all.contains("echo hi"));

    // Idempotent: a second init does not add a duplicate hook.
    e.cmd().arg("init").assert().success();
    let v2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.settings()).unwrap()).unwrap();
    assert_eq!(v2["hooks"]["Stop"].as_array().unwrap().len(), 2);
}

#[test]
fn init_says_what_it_is_about_to_record() {
    let e = Env::new();
    e.cmd()
        .args(["init", "--no-hook"])
        .assert()
        .success()
        .stdout(predicate::str::contains("What gets recorded"))
        .stdout(predicate::str::contains(
            "nothing at all leaves this machine",
        ))
        .stdout(predicate::str::contains("tmem forget"));
}

#[test]
fn init_backfill_imports_what_is_already_on_disk() {
    let e = Env::new();
    e.install("finding-09-many-to-one.jsonl");
    e.install("finding-05-two-threads.jsonl");
    e.cmd()
        .args(["init", "--backfill", "--no-hook"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4 exchanges from 2 transcripts"));
}

#[test]
fn doctor_reports_an_unwired_capture_path() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.cmd()
        .arg("doctor")
        .assert()
        .code(2)
        .stdout(predicate::str::contains("no Stop hook"));
}

#[test]
fn doctor_notices_a_transcript_that_was_never_ingested() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.install("finding-09-many-to-one.jsonl");
    e.cmd()
        .arg("doctor")
        .assert()
        .stdout(predicate::str::contains("never ingested"));
}

#[test]
fn status_counts_what_is_there() {
    let e = Env::new();
    e.ingest("finding-05-two-threads.jsonl");
    e.cmd()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("exchanges   3"))
        .stdout(predicate::str::contains("threads     2"))
        .stdout(predicate::str::contains("capture     ON"));
}

/// docs/cli.md makes search the default verb, but it lands in Phase 2. Saying
/// so is the point: an empty result would look like a lost exchange.
#[test]
fn a_bare_query_says_search_is_not_here_yet() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    e.cmd()
        .args(["ffmpeg", "concat"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Phase 2"));
}

#[test]
fn the_hook_reports_a_payload_without_a_transcript_rather_than_failing_the_turn() {
    let e = Env::new();
    e.cmd()
        .args(["capture", "--hook", "claude-code"])
        .env("TMEM_NO_SPAWN", "1")
        .write_stdin(r#"{"session_id":"s","cwd":"/home/dev"}"#)
        .assert()
        .success()
        .stderr(predicate::str::contains("no transcript_path"));
}

#[test]
fn a_malformed_transcript_line_does_not_stop_the_ingest() {
    let e = Env::new();
    e.ingest("malformed-line.jsonl");
    assert_eq!(e.count("exchanges"), 1);
}
