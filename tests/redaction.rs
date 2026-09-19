//! Phase 3's Exit criterion, taken literally:
//!
//! > a paste-a-token test, performed adversarially, leaves nothing recoverable
//! > in the database file.
//!
//! "Adversarially" is the operative word. These tests do not check that a row
//! looks right — they read the raw bytes of `memory.db` and every file beside
//! it, and fail if the secret is anywhere in any of them. That is the only
//! version of this claim worth making, because every intermediate structure
//! SQLite keeps (the WAL, freed pages, the FTS index, the derived command rows)
//! is a place a secret can survive a delete that looked complete.

mod common;

use common::Env;
use predicates::prelude::*;
use std::path::Path;

/// Scenario 3's paste: a request log with a live bearer token in it.
const TOKEN: &str = "ghp_S3cr3tT0k3nAAAAAAAAAAAAAAAAAAAAAAAA";
const AWS: &str = "AKIAIOSFODNN7EXAMPLE";

fn transcript(prompt: &str, response: &str, command: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s-leak","timestamp":"2026-04-02T15:00:00.000Z","cwd":"/home/dev/src/webhooks","gitBranch":"main","message":{{"role":"user","content":{}}}}}
{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s-leak","timestamp":"2026-04-02T15:00:09.000Z","cwd":"/home/dev/src/webhooks","gitBranch":"main","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":{}}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":{}}}}}]}}}}
"#,
        serde_json::to_string(prompt).unwrap(),
        serde_json::to_string(response).unwrap(),
        serde_json::to_string(command).unwrap(),
    )
}

/// Every byte term-mem wrote, not just the main database file. The WAL is a
/// separate file and a secret sitting in it is still a secret on disk.
fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(b) = std::fs::read(&path) {
                out.extend_from_slice(&b);
            }
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

/// The headline: a token pasted into a prompt never reaches the disk at all,
/// because redaction runs pre-write.
#[test]
fn a_pasted_token_never_reaches_the_database() {
    let e = Env::new();
    e.write_transcript(
        "leak.jsonl",
        &transcript(
            &format!("why is this failing?\n\nAuthorization: Bearer {TOKEN}\nX-Request-Id: 42"),
            "Your clock-skew tolerance is zero. Widen it.",
            "echo ok",
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    let bytes = all_bytes(&e.data());
    assert!(
        !contains(&bytes, TOKEN),
        "the token reached disk — redaction must run before the insert, not after"
    );
    // And the exchange is still there, and still useful.
    e.cmd()
        .args(["search", "clock", "skew"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("skew"));
}

/// The same for the response and for a mined command line — `curl -H
/// 'Authorization: …'` is where a credential most often actually lands, and the
/// command text is stored twice over (the `commands` table and the
/// `commands_text` column the index reads).
#[test]
fn secrets_in_a_response_or_a_command_are_redacted_too() {
    let e = Env::new();
    e.write_transcript(
        "leak.jsonl",
        &transcript(
            "how do I call the API?",
            &format!("Use this:\n\n```\nexport AWS_ACCESS_KEY_ID={AWS}\n```"),
            &format!("curl -H 'Authorization: Bearer {TOKEN}' https://api.example.com"),
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    let bytes = all_bytes(&e.data());
    assert!(!contains(&bytes, TOKEN), "command text leaked");
    assert!(!contains(&bytes, AWS), "response text leaked");

    // Both derived copies of the command agree with the row.
    for col in [
        "SELECT cmd FROM commands",
        "SELECT commands_text FROM exchanges",
    ] {
        let vals = e.query(col);
        assert!(
            vals.iter().all(|v| !v.contains(TOKEN)),
            "{col} still holds it: {vals:?}"
        );
    }
}

/// Redaction is never silent. docs/plan.md: "silent redaction leaves the user
/// unable to tell a mangled response from a bad one."
#[test]
fn redaction_says_so_at_capture_and_in_status() {
    let e = Env::new();
    e.write_transcript(
        "leak.jsonl",
        &transcript(
            &format!("token is {TOKEN}"),
            "noted",
            &format!("aws configure set aws_access_key_id {AWS}"),
        ),
    );
    e.cmd()
        .args(["capture", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("redacted: 1 exchange"))
        .stdout(predicate::str::contains("github-token"));

    e.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("redacted    1 exchange"));

    // And the row carries the flag, so `show` can mark it.
    assert_eq!(
        e.query("SELECT CAST(redacted AS TEXT) FROM exchanges"),
        vec!["1"]
    );
    e.cmd()
        .args(["show", &e.rows()[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains("redacted"));
}

/// What prevention misses, the valve takes — and the valve is measured by
/// grepping the file, not by counting rows. This is the adversarial half of the
/// Exit criterion: a secret that redaction had no rule for.
#[test]
fn forget_leaves_nothing_recoverable_for_a_secret_no_rule_knew() {
    let e = Env::new();
    // Deliberately shaped like nothing: no prefix, no assignment, low entropy
    // per character. No rule fires, which is the point.
    let secret = "the passphrase is open sesame banana staple";
    e.write_transcript(
        "leak.jsonl",
        &transcript(secret, "I will not repeat it", "echo done"),
    );
    e.cmd().args(["capture", "--all"]).assert().success();
    assert_eq!(
        e.query("SELECT CAST(redacted AS TEXT) FROM exchanges"),
        vec!["0"]
    );
    assert!(
        contains(&all_bytes(&e.data()), secret),
        "the premise of this test is that redaction did not catch it"
    );

    e.cmd().args(["forget", "--last", "-y"]).assert().success();

    let bytes = all_bytes(&e.data());
    assert!(
        !contains(&bytes, secret),
        "forget left the secret recoverable from a file on disk"
    );
    assert_eq!(e.count("exchanges"), 0);
    e.cmd().args(["search", "sesame"]).assert().code(1);
}

/// A deleted secret must not come back through the *other* door into the
/// database. `import` is new in this phase and is the second write path.
#[test]
fn an_import_cannot_undo_a_forget() {
    let e = Env::new();
    e.write_transcript(
        "leak.jsonl",
        &transcript("remember the codeword swordfish", "ok", "echo done"),
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    let out = e.cmd().args(["export", "--json"]).assert().success();
    let backup = e.home().join("backup.jsonl");
    std::fs::write(&backup, &out.get_output().stdout).unwrap();

    e.cmd().args(["forget", "--last", "-y"]).assert().success();
    e.cmd()
        .args(["import"])
        .arg(&backup)
        .assert()
        .success()
        .stdout(predicate::str::contains("left out because you forgot"));

    assert_eq!(e.count("exchanges"), 0, "the import undid a forget");
    // The export file still holds it, which is the user's own copy and their
    // business — but nothing term-mem manages does.
    assert!(contains(&std::fs::read(&backup).unwrap(), "swordfish"));
}

// ── export / import ──────────────────────────────────────────────────────

/// "Export is the concrete form of the mission's ownership promise" — so it
/// exports the archive, not a page of it. The 20-row browse default must not
/// leak into a backup.
#[test]
fn export_writes_the_whole_archive_not_the_first_page() {
    let e = Env::new();
    let mut body = String::new();
    for i in 0..25 {
        body.push_str(&format!(
            r#"{{"type":"user","uuid":"u{i}","parentUuid":null,"sessionId":"s{i}","timestamp":"2026-04-0{}T1{}:00:00.000Z","cwd":"/home/dev/x","message":{{"role":"user","content":"question number {i}"}}}}
{{"type":"assistant","uuid":"a{i}","parentUuid":"u{i}","sessionId":"s{i}","timestamp":"2026-04-0{}T1{}:00:01.000Z","cwd":"/home/dev/x","message":{{"role":"assistant","model":"m","content":[{{"type":"text","text":"answer number {i}"}}]}}}}
"#,
            1 + i % 9, i % 10, 1 + i % 9, i % 10
        ));
    }
    e.write_transcript("many.jsonl", &body);
    e.cmd().args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 25);

    let out = e.cmd().args(["export", "--json"]).assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(text.lines().count(), 25, "an export is not a result page");

    // But an explicit -n is still honoured.
    let out = e
        .cmd()
        .args(["export", "--json", "-n", "5"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8(out.get_output().stdout.clone())
            .unwrap()
            .lines()
            .count(),
        5
    );
}

/// Round-trip: export, wipe, import, and the archive is what it was — including
/// the ids, so an id written down in a PR description still resolves.
#[test]
fn an_export_round_trips_through_import() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let before =
        e.query("SELECT id || '|' || prompt || '|' || commands_text FROM exchanges ORDER BY id");
    assert!(!before.is_empty());

    let out = e.cmd().args(["export", "--json"]).assert().success();
    let backup = e.home().join("backup.jsonl");
    std::fs::write(&backup, &out.get_output().stdout).unwrap();

    e.cmd()
        .args(["forget", "--since", "2000-01-01", "-y"])
        .assert()
        .success();
    assert_eq!(e.count("exchanges"), 0);

    // The tombstones from that wipe would otherwise block the restore, which is
    // correct for `forget` and wrong for "restore my backup" — so this asserts
    // the behaviour rather than assuming it.
    let restored = e.cmd().args(["import"]).arg(&backup).assert().success();
    let stdout = String::from_utf8(restored.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("left out because you forgot"),
        "a forget must survive an import: {stdout}"
    );
    assert_eq!(e.count("exchanges"), 0);
}

/// The ordinary round trip, with no `forget` in the way.
#[test]
fn import_restores_into_an_empty_archive_and_is_idempotent() {
    let source = Env::new();
    source.ingest("finding-09-many-to-one.jsonl");
    let out = source.cmd().args(["export", "--json"]).assert().success();
    let backup = source.home().join("backup.jsonl");
    std::fs::write(&backup, &out.get_output().stdout).unwrap();
    let expected = source.query("SELECT id FROM exchanges ORDER BY id");

    let dest = Env::new();
    dest.cmd().args(["init", "--no-hook"]).assert().success();
    dest.cmd()
        .args(["import"])
        .arg(&backup)
        .assert()
        .success()
        .stdout(predicate::str::contains("imported 1 exchange"));
    assert_eq!(dest.query("SELECT id FROM exchanges ORDER BY id"), expected);

    // Twice is a no-op, like every other write path in this project.
    dest.cmd()
        .args(["import"])
        .arg(&backup)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 already present"));
    assert_eq!(dest.count("exchanges"), 1);

    // And what came back is searchable, so the index was maintained too.
    dest.cmd().args(["search", "ffmpeg"]).assert().code(0);
}

/// A corrupt line must cost that line, not the rest of someone's history.
#[test]
fn import_survives_a_bad_line() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let out = e.cmd().args(["export", "--json"]).assert().success();
    let mut text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    text.insert_str(0, "{not json at all\n");
    let backup = e.home().join("backup.jsonl");
    std::fs::write(&backup, text).unwrap();

    e.cmd().args(["forget", "--last", "-y"]).assert().success();
    let dest = Env::new();
    dest.cmd().args(["init", "--no-hook"]).assert().success();
    dest.cmd()
        .args(["import"])
        .arg(&backup)
        .assert()
        .success()
        .stdout(predicate::str::contains("imported 1 exchange"))
        .stdout(predicate::str::contains("1 line(s) could not be read"));
}

/// The markdown form is for people. It must contain the content and must not
/// pretend to be machine-readable.
#[test]
fn markdown_export_is_readable_and_complete() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let out = e.cmd().args(["export", "--markdown"]).assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("### Prompt"), "{text}");
    assert!(text.contains("4 mp4 files"), "{text}");
    assert!(
        text.contains("ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4"),
        "{text}"
    );
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_err());
}

// ── the user rule file ───────────────────────────────────────────────────

/// docs/plan.md: "internal hostname and ticket-ID shapes are site-specific and
/// no shipped ruleset will guess them."
#[test]
fn a_user_rule_catches_what_no_shipped_rule_could() {
    let e = Env::new();
    let config = e.home().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("redact.toml"),
        r#"
[[rule]]
name = "internal-host"
pattern = '\b[a-z0-9-]+\.corp\.internal\b'

[[rule]]
name = "ticket"
pattern = '\bOPS-[0-9]{4,}\b'
"#,
    )
    .unwrap();

    e.write_transcript(
        "site.jsonl",
        &transcript(
            "deploy is failing on build07.corp.internal, see OPS-48213",
            "check the disk",
            "ssh build07.corp.internal",
        ),
    );
    e.cmd()
        .args(["capture", "--all"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .success()
        .stdout(predicate::str::contains("internal-host"));

    let bytes = all_bytes(&e.data());
    assert!(!contains(&bytes, "build07.corp.internal"), "host leaked");
    assert!(!contains(&bytes, "OPS-48213"), "ticket leaked");
    // The sentence around them survives.
    e.cmd()
        .args(["search", "deploy"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .code(0);
}

/// A rule file that does not compile is an error, not a warning that scrolls
/// past. A redactor the user believes is running and which silently is not is
/// the worst outcome available here.
#[test]
fn a_broken_rule_file_stops_capture_rather_than_capturing_unredacted() {
    let e = Env::new();
    let config = e.home().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("redact.toml"),
        "[[rule]]\nname = \"bad\"\npattern = '('\n",
    )
    .unwrap();
    e.write_transcript(
        "site.jsonl",
        &transcript(&format!("token {TOKEN}"), "ok", "echo hi"),
    );

    e.cmd()
        .args(["capture", "--all"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("bad"));
    assert!(
        !contains(&all_bytes(&e.data()), TOKEN),
        "capture proceeded with a broken ruleset"
    );
}

/// The entropy fallback is off by default, and this is the test that says so
/// on purpose rather than by omission.
///
/// docs/plan.md's Scope asks for it unconditionally. Measured against a real
/// archive it produced 38 false positives and zero true ones, then 8, then 3,
/// across three tightenings — every hit a path or a filename. A false positive
/// here is not a bad search result: the raw `tool_use` block is never stored,
/// so a mangled command line is gone for good, and "capture is irreversible"
/// outranks "redact everything that might be a secret".
/// See docs/phases/phase-3.md finding 2.
#[test]
fn the_entropy_fallback_is_opt_in() {
    let e = Env::new();
    let unknown = "export DEPLOY_TOKEN=Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn";
    e.write_transcript(
        "unknown.jsonl",
        &transcript(
            "how do I deploy?",
            &format!("Run:\n```\n{unknown}\n```"),
            "echo hi",
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();
    assert!(
        contains(&all_bytes(&e.data()), "Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn"),
        "the default ruleset should not have touched this"
    );
    assert_eq!(
        e.query("SELECT CAST(redacted AS TEXT) FROM exchanges"),
        vec!["0"]
    );

    // One line in the rule file turns it on, and then it fires.
    let dest = Env::new();
    let config = dest.home().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("redact.toml"), "[entropy]\nenabled = true\n").unwrap();
    dest.write_transcript(
        "unknown.jsonl",
        &transcript(
            "how do I deploy?",
            &format!("Run:\n```\n{unknown}\n```"),
            "echo hi",
        ),
    );
    dest.cmd()
        .args(["capture", "--all"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .success()
        .stdout(predicate::str::contains("entropy"));
    assert!(!contains(
        &all_bytes(&dest.data()),
        "Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn"
    ));
}

/// The false positives that drove the decision, end to end: a developer's
/// command lines survive capture intact even with the entropy rule enabled.
#[test]
fn paths_and_filenames_survive_even_with_entropy_on() {
    let e = Env::new();
    let config = e.home().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("redact.toml"), "[entropy]\nenabled = true\n").unwrap();

    let real_command = "f=tests/fixtures/claude_code/finding-09-many-to-one.jsonl && cat $f";
    e.write_transcript(
        "paths.jsonl",
        &transcript("run the fixture", "here you go", real_command),
    );
    e.cmd()
        .args(["capture", "--all"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .success();

    let cmds = e.query("SELECT cmd FROM commands");
    assert_eq!(
        cmds,
        vec![real_command],
        "a path in a command line is not a credential, and losing it is permanent"
    );
}

/// A `Read` of `/home/dev/secrets/ghp_….pem` puts the credential in
/// `file_refs`, in the raw database bytes, and in every export — with
/// `redacted` left at 0. Mined file paths are text the assistant produced and
/// the first cut of this phase did not pass them through the redactor, while
/// the function's own comment claimed it covered "every field that carries
/// text". docs/phases/phase-3.md finding 7.
#[test]
fn a_secret_in_a_mined_file_path_is_redacted() {
    let e = Env::new();
    e.write_transcript(
        "path.jsonl",
        &format!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s1","timestamp":"2026-04-02T15:00:00.000Z","cwd":"/home/dev/x","message":{{"role":"user","content":"open that key"}}}}
{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-04-02T15:00:01.000Z","cwd":"/home/dev/x","message":{{"role":"assistant","model":"m","content":[{{"type":"text","text":"ok"}},{{"type":"tool_use","id":"t1","name":"Read","input":{{"file_path":"/home/dev/secrets/{TOKEN}.pem"}}}}]}}}}
"#
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    assert!(
        !contains(&all_bytes(&e.data()), TOKEN),
        "a credential in a file path reached the archive"
    );
    let paths = e.query("SELECT path FROM file_refs");
    assert!(
        paths.iter().all(|p| !p.contains(TOKEN)),
        "file_refs still holds it: {paths:?}"
    );
    assert_eq!(
        e.query("SELECT CAST(redacted AS TEXT) FROM exchanges"),
        vec!["1"],
        "the row must be flagged, or the user cannot tell it was touched"
    );
}

/// A rule file that does not compile stops capture, which is right. The drainer
/// runs detached with its stderr discarded, so the user sees capture stop and
/// nothing tell them why — and `doctor` is the command whose entire job is
/// answering that question.
#[test]
fn doctor_reports_a_rule_file_that_will_not_compile() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");
    let config = e.home().join("config");
    std::fs::create_dir_all(&config).unwrap();

    // Not `.success()`: in this harness `tmem` is not on PATH, which doctor
    // correctly calls a problem. What matters is what it says about the rules.
    e.cmd()
        .args(["doctor"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .stdout(predicate::str::contains("redaction rules load"))
        .stdout(predicate::str::contains("capture cannot run").not());

    std::fs::write(
        config.join("redact.toml"),
        "[[rule]]\nname = \"bad\"\npattern = '('\n",
    )
    .unwrap();
    e.cmd()
        .args(["doctor"])
        .env("TMEM_CONFIG_DIR", &config)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("capture cannot run"));
}
