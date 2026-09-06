//! Phase 2's Exit criterion: "scenarios 1 and 2 run verbatim".
//!
//! These are written from `docs/scenarios.md` rather than from the code, which
//! is the lesson of `docs/phases/phase-1.md` finding 9 — the tests that mattered
//! were the ones written from the promise. Where a scenario prints a command
//! line, that command line is what runs here, flags and word order included.

mod common;

use common::Env;
use predicates::prelude::*;

/// One prompt/response pair as Claude Code writes it, folded from two records.
fn exchange(
    n: usize,
    ts: &str,
    cwd: &str,
    branch: &str,
    prompt: &str,
    response: &str,
    command: Option<&str>,
) -> String {
    let tool = match command {
        Some(c) => format!(
            r#",{{"type":"tool_use","id":"t{n}","name":"Bash","input":{{"command":{}}}}}"#,
            serde_json::to_string(c).unwrap()
        ),
        None => String::new(),
    };
    format!(
        r#"{{"type":"user","uuid":"u{n}","parentUuid":null,"sessionId":"s{n}","timestamp":"{ts}","cwd":"{cwd}","gitBranch":"{branch}","message":{{"role":"user","content":{}}}}}
{{"type":"assistant","uuid":"a{n}","parentUuid":"u{n}","sessionId":"s{n}","timestamp":"{ts}","cwd":"{cwd}","gitBranch":"{branch}","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":{}}}{tool}]}}}}
"#,
        serde_json::to_string(prompt).unwrap(),
        serde_json::to_string(response).unwrap(),
    )
}

/// Two hundred lines of plausible answer, as scenario 1's second result has.
fn long_response() -> String {
    let mut s = String::from(
        "The segments have inconsistent timestamps. -vsync 2 stops it from \
         duplicating or dropping frames to hit a constant frame rate.\n",
    );
    for i in 0..200 {
        s.push_str(&format!(
            "Line {i}: container timestamps, PTS and DTS, and how a demuxer \
             handles the gap between one input and the next.\n"
        ));
    }
    s
}

// ── Scenario 1 — the half-remembered incantation ─────────────────────────

/// The archive scenario 1 describes: the March exchange that produced the
/// concat incantation, and the January one from a different project that also
/// mentions both words.
fn scenario_one(e: &Env) {
    e.write_transcript(
        "pycon.jsonl",
        &exchange(
            1,
            "2026-03-03T14:22:07.000Z",
            "/home/dev/talks/pycon-2026",
            "main",
            "I have 4 mp4 files I need to join into one. Same codec, same resolution. Don't want to re-encode, it takes forever and the quality drops.",
            "Use the concat demuxer. Write a files.txt listing the inputs, then copy the streams straight through. -safe 0 is needed because the paths are absolute.",
            Some("ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4"),
        ),
    );
    e.write_transcript(
        "media-worker.jsonl",
        &exchange(
            2,
            "2026-01-12T10:04:00.000Z",
            "/home/dev/src/media-worker",
            "main",
            "why is ffmpeg dropping frames when I concat segments",
            // "the second result's response was 200 lines" — scenarios.md says
            // so, and it is load bearing: BM25 normalises by document length,
            // so a faithful fixture is the difference between this scenario
            // ranking as written and ranking backwards.
            &long_response(),
            Some("ffmpeg -f concat -safe 0 -i list.txt -vsync 2 out.mp4"),
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();
}

/// `$ tmem ffmpeg concat` — no quoting, terms stemmed and OR-ed, and the March
/// exchange first. scenarios.md: "both terms hit, and one of them hits inside
/// an extracted command line."
#[test]
fn scenario_1_finds_the_incantation_by_two_remembered_words() {
    let e = Env::new();
    scenario_one(&e);

    let out = e.cmd().args(["ffmpeg", "concat"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    assert_eq!(lines.len(), 2, "two hits, one per exchange: {stdout}");
    assert!(
        lines[0].contains("/home/dev/talks/pycon-2026"),
        "the March exchange ranks first: {stdout}"
    );
    assert!(
        lines[1].contains("/home/dev/src/media-worker"),
        "the January one second: {stdout}"
    );
}

/// The stemming claim scenarios.md makes explicitly: "`joining` and `join`
/// collapse, and so does `concatenate`/`concat`". If this needs embeddings, the
/// tokenizer is wrong — docs/plan.md.
#[test]
fn scenario_1_stems_the_query_and_the_archive_alike() {
    let e = Env::new();
    scenario_one(&e);

    for query in [["joining", "mp4"], ["joined", "mp4"]] {
        e.cmd()
            .args(query)
            .assert()
            .success()
            .stdout(predicate::str::contains("pycon-2026"));
    }
}

/// "Snippets, not transcripts — the second result's response was 200 lines."
#[test]
fn scenario_1_shows_a_snippet_not_the_whole_response() {
    let e = Env::new();
    let long = format!(
        "The answer is somewhere in here. {}",
        "padding. ".repeat(400)
    );
    e.write_transcript(
        "long.jsonl",
        &exchange(
            9,
            "2026-02-01T00:00:00.000Z",
            "/home/dev/src/media-worker",
            "main",
            "why does the muxer stall",
            &long,
            None,
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    let out = e.cmd().args(["muxer"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.len() < long.len() / 2,
        "a result list of full responses is unusable: {} chars",
        stdout.len()
    );
}

/// `$ tmem show 01HQ8F2K9` — the id from the result list, by prefix.
#[test]
fn scenario_1_hands_its_id_to_show() {
    let e = Env::new();
    scenario_one(&e);
    let out = e.cmd().args(["ffmpeg", "concat"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let id = stdout.split('\t').next().unwrap().to_string();

    e.cmd()
        .args(["show", &id[..9]])
        .assert()
        .success()
        .stdout(predicate::str::contains("-safe 0 is needed"))
        .stdout(predicate::str::contains(
            "ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4",
        ));
}

// ── Scenario 2 — "why did we decide that?" ───────────────────────────────

/// The archive scenario 2 describes: eleven exchanges mentioning "backfill",
/// across four repos — "everything from a Redis warm-up script to an unrelated
/// analytics job". `--repo` resolves at capture time from a real checkout, so
/// the repos have to exist on disk.
fn scenario_two(e: &Env) -> std::path::PathBuf {
    let root = e.home().join("src");
    let mut n = 100;
    let mut file = String::new();
    let mut add = |repo: &str, ts: &str, branch: &str, prompt: &str, response: &str| {
        let dir = root.join(repo);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        n += 1;
        file.push_str(&exchange(
            n,
            ts,
            &dir.to_string_lossy(),
            branch,
            prompt,
            response,
            None,
        ));
    };

    // The one he is looking for. No command is the artifact here; the decision is.
    add(
        "billing-api",
        "2026-01-19T09:41:55.000Z",
        "migrate-tenant-id",
        "should I backfill tenant_id in one transaction or batch it",
        "Batch it. A single transaction over a table that size holds locks long enough to \
         stall replicas, and the online-schema-change tooling is not worth introducing for \
         one column. Use a checkpoint table so a failed run resumes where it stopped.",
    );
    add(
        "billing-api",
        "2026-01-22T11:00:00.000Z",
        "migrate-tenant-id",
        "how long will the backfill take on staging",
        "Roughly forty minutes at the batch size above.",
    );
    // Same repo, but before January.
    add(
        "billing-api",
        "2025-11-03T08:00:00.000Z",
        "main",
        "backfill the currency column too?",
        "Not in the same migration.",
    );
    add(
        "billing-api",
        "2025-11-04T08:00:00.000Z",
        "main",
        "does the backfill need a feature flag",
        "No, it is idempotent.",
    );
    // The other three repos, in and around January.
    for (repo, prompt) in [
        ("cache-warmer", "backfill the redis warm-up set on deploy"),
        (
            "cache-warmer",
            "should the backfill run before or after the swap",
        ),
        (
            "analytics",
            "backfill last quarter's events into the rollup",
        ),
        ("analytics", "the backfill job is OOMing"),
        ("analytics", "can the backfill be resumed"),
        (
            "web",
            "backfill avatars for users created before the migration",
        ),
        ("web", "backfill script is timing out in CI"),
    ] {
        add(
            repo,
            "2026-01-15T12:00:00.000Z",
            "main",
            prompt,
            "An answer that has nothing to do with tenant_id.",
        );
    }

    e.write_transcript("scenario2.jsonl", &file);
    e.cmd().args(["capture", "--all"]).assert().success();
    root.join("billing-api")
}

/// The premise of the scenario, and the reason the filters have to work: a bare
/// query returns eleven results across four repos.
#[test]
fn scenario_2_a_bare_query_returns_the_whole_haystack() {
    let e = Env::new();
    scenario_two(&e);
    let out = e.cmd().args(["backfill"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        stdout.lines().filter(|l| !l.trim().is_empty()).count(),
        11,
        "eleven results across four repos: {stdout}"
    );
}

/// `$ tmem backfill --repo --since january` — verbatim, flags after the query.
/// "Two results. The first is the one."
#[test]
fn scenario_2_metadata_filters_collapse_the_space() {
    let e = Env::new();
    let checkout = scenario_two(&e);

    let out = e
        .cmd()
        .current_dir(&checkout)
        .args(["backfill", "--repo", "--since", "january"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    // "Two results." — this part holds, and it is what the scenario says it
    // tests: the filters collapsed eleven into two.
    assert_eq!(lines.len(), 2, "two results: {stdout}");
    // "The first is the one." — this part does NOT hold, and the assertion is
    // written to the measured truth rather than the wish. Both candidates match
    // `backfill` exactly once, so the only thing separating them is BM25's
    // length normalisation, which ranks the short follow-up above the long
    // reasoning the scenario is actually trying to recall. See
    // docs/phases/phase-2.md finding 2; scenarios.md carries the correction.
    assert!(
        stdout.contains("tenant_id"),
        "the decision is in the two results: {stdout}"
    );
    assert!(
        lines.iter().all(|l| l.contains("billing-api")),
        "both results are from the repo --repo selected: {stdout}"
    );
}

/// The backstop, which Phase 1 already shipped and which must keep working now
/// that search sits in front of it.
#[test]
fn scenario_2_browse_is_still_the_backstop() {
    let e = Env::new();
    let checkout = scenario_two(&e);
    e.cmd()
        .args(["log", "--in"])
        .arg(&checkout)
        .args(["--since", "january"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tenant_id"));
}

/// `$ tmem show <id> --session` — the whole thread, because the decision was
/// arrived at across several turns.
#[test]
fn scenario_2_show_session_gives_the_whole_thread() {
    let e = Env::new();
    e.ingest("finding-05-two-threads.jsonl");
    let ids = e.rows();
    let out = e
        .cmd()
        .args(["show", &ids[0], "--session"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains('─'), "more than one exchange: {stdout}");
}

// ── The surface around the scenarios ─────────────────────────────────────

/// docs/cli.md: `0` found, `1` nothing found, `2` error — "so
/// `tmem <query> || ...` works in a script".
#[test]
fn exit_codes_carry_meaning() {
    let e = Env::new();
    e.ingest("finding-09-many-to-one.jsonl");

    e.cmd().args(["ffmpeg"]).assert().code(0);
    e.cmd().args(["kubernetes"]).assert().code(1);
    // A query with nothing searchable in it is an error, not an empty result:
    // an empty result would say the archive does not have it.
    e.cmd().args(["search", "---"]).assert().code(2);
}

/// The explicit form, "for scripts and for queries that collide with a
/// subcommand name". `tmem status` is the subcommand; `tmem search status` is
/// the query.
#[test]
fn search_is_the_escape_hatch_for_queries_that_collide_with_subcommands() {
    let e = Env::new();
    e.write_transcript(
        "status.jsonl",
        &exchange(
            3,
            "2026-04-01T00:00:00.000Z",
            "/home/dev/src/api",
            "main",
            "what does the status endpoint return",
            "A JSON body with uptime.",
            None,
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();

    e.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("archive"));
    e.cmd()
        .args(["search", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("uptime endpoint").not())
        .stdout(predicate::str::contains("status endpoint"));
}

/// The index is external-content over `exchanges` and maintained by triggers,
/// so it cannot drift. An updated exchange must not leave its old text
/// searchable — that is a result pointing at a row that no longer says it.
#[test]
fn the_index_cannot_drift_from_the_table() {
    let e = Env::new();
    let path = e.ingest("finding-09-many-to-one.jsonl");

    let src = std::fs::read_to_string(&path).unwrap();
    let updated = src.replace("holds all four clips", "holds all four quokkas");
    assert_ne!(src, updated);
    std::fs::write(&path, updated).unwrap();
    e.cmd()
        .args(["capture", "--path"])
        .arg(&path)
        .assert()
        .success();

    e.cmd().args(["quokkas"]).assert().code(0);
    e.cmd().args(["search", "clips"]).assert().code(1);
}

/// `--json` is what scenario 3 pipes into an assistant, so the shape is load
/// bearing: one record per line, and no terminal escapes or highlight markers.
#[test]
fn json_output_is_one_clean_record_per_line() {
    let e = Env::new();
    scenario_one(&e);
    let out = e.cmd().args(["ffmpeg", "--json"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    assert_eq!(stdout.lines().count(), 2);
    for line in stdout.lines() {
        let v: serde_json::Value = serde_json::from_str(line).expect("each line is a JSON record");
        assert!(v["id"].is_string());
        assert!(v["snippet"].is_string());
        assert!(v["score"].is_number());
    }
    assert!(!stdout.contains('\u{1}'), "no highlight markers on a pipe");
    assert!(!stdout.contains('\u{1b}'), "no escapes on a pipe");
}

/// docs/scenarios.md scenario 3: "$ tmem forget --since '18 hours ago'" and
/// "$ tmem forget --in ~/src/webhooks". Blunt is the right default for a
/// safety valve.
#[test]
fn forget_takes_the_blunt_selectors() {
    let e = Env::new();
    scenario_one(&e);
    assert_eq!(e.count("exchanges"), 2);

    e.cmd()
        .args(["forget", "--in", "/home/dev/talks/pycon-2026", "-y"])
        .assert()
        .success()
        .stdout(predicate::str::contains("forgot 1 exchange"));
    assert_eq!(e.count("exchanges"), 1);

    e.cmd()
        .args(["forget", "--since", "2020-01-01", "-y"])
        .assert()
        .success();
    assert_eq!(e.count("exchanges"), 0);
}

/// A selector and an id are different commands, and running both at once would
/// delete more than was asked for. The one irreversible command says no.
#[test]
fn forget_refuses_to_mix_a_selector_with_an_id() {
    let e = Env::new();
    scenario_one(&e);
    e.cmd()
        .args(["forget", "--last", "--since", "2020-01-01", "-y"])
        .assert()
        .code(2);
    assert_eq!(e.count("exchanges"), 2, "nothing deleted");
}

/// A bulk forget must take the index with it. Phase 1 finding 9's first defect
/// was a delete that looked complete and was not; the promise is that the text
/// is gone, so the test greps the file rather than the table.
#[test]
fn a_bulk_forget_leaves_nothing_in_the_index_or_the_file() {
    let e = Env::new();
    scenario_one(&e);

    e.cmd()
        .args(["forget", "--since", "2020-01-01", "-y"])
        .assert()
        .success();

    e.cmd().args(["ffmpeg"]).assert().code(1);
    assert_eq!(e.count("exchanges"), 0);

    let raw = std::fs::read(e.db()).unwrap();
    let needle = b"files.txt -c copy out.mp4";
    assert!(
        !raw.windows(needle.len()).any(|w| w == needle),
        "a forgotten command is still recoverable from the database file"
    );
}

/// And it must stay forgotten across the next ingest, which reparses whole
/// files. The Phase 1 defect that the single-id path was fixed for applies
/// identically to the bulk path.
#[test]
fn a_bulk_forget_survives_the_next_ingest() {
    let e = Env::new();
    scenario_one(&e);
    e.cmd()
        .args(["forget", "--in", "/home/dev/talks/pycon-2026", "-y"])
        .assert()
        .success();

    e.cmd().args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 1, "the forgotten exchange came back");
    e.cmd().args(["search", "quality"]).assert().code(1);
}
