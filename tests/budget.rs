//! docs/plan.md Phase 1: **Budget: hook latency under 5ms. Measured, not
//! assumed — a hook on the turn boundary is in the user's way by construction.**
//!
//! Run in release, which is what a user has:
//!
//! ```text
//! cargo test --release --test budget -- --nocapture
//! ```
//!
//! What is measured is the whole cost the user pays: process start, the pause
//! and ignore checks, reading the payload, writing the queue entry, exit. The
//! parse and the database write are deliberately not in it — that is the point
//! of the queue.

mod common;

use common::Env;
use std::time::Instant;

const BUDGET_MS: f64 = 5.0;
const ITERATIONS: usize = 60;

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn measure(e: &Env, transcript: &std::path::Path) -> Vec<f64> {
    let payload = format!(
        r#"{{"session_id":"s","cwd":"/home/dev","transcript_path":"{}"}}"#,
        transcript.display()
    );
    let mut samples = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS + 5 {
        let t = Instant::now();
        e.cmd()
            .args(["capture", "--hook", "claude-code"])
            .env("TMEM_NO_SPAWN", "1")
            .write_stdin(payload.clone())
            .assert()
            .success();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if i >= 5 {
            samples.push(ms); // discard warm-up
        }
    }
    samples.sort_by(f64::total_cmp);
    samples
}

#[test]
fn hook_latency_is_under_five_milliseconds() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let small = e.install("finding-09-many-to-one.jsonl");

    // And a transcript far larger than anything the Phase 0 sample contained,
    // to show the hook's cost does not track the file it points at.
    let big = e.projects().join("proj/big.jsonl");
    let one = std::fs::read_to_string(&small).unwrap();
    let mut bulk = String::new();
    while bulk.len() < 8 * 1024 * 1024 {
        bulk.push_str(&one);
    }
    std::fs::write(&big, &bulk).unwrap();

    let s = measure(&e, &small);
    let b = measure(&e, &big);

    let report = |name: &str, v: &[f64]| {
        println!(
            "  {name:<24} p50 {:.2} ms   p95 {:.2} ms   max {:.2} ms",
            percentile(v, 0.50),
            percentile(v, 0.95),
            v[v.len() - 1]
        );
    };
    println!("\nStop hook latency ({ITERATIONS} samples, release build):");
    report("12-record transcript", &s);
    report(&format!("{} MB transcript", bulk.len() / 1024 / 1024), &b);
    println!();

    let p95_small = percentile(&s, 0.95);
    let p95_big = percentile(&b, 0.95);
    assert!(
        p95_small < BUDGET_MS,
        "hook p95 was {p95_small:.2} ms against a {BUDGET_MS} ms budget"
    );
    assert!(
        p95_big < BUDGET_MS,
        "hook p95 on a large transcript was {p95_big:.2} ms against a {BUDGET_MS} ms budget \
         — the hook is doing work proportional to the file, which it must not"
    );
}

// ── Phase 2 ──────────────────────────────────────────────────────────────
//
// docs/plan.md: "p95 query latency under 100ms on 100k exchanges — generate the
// synthetic archive to prove it rather than waiting to be surprised in year
// two." docs/tech-stack.md adds the reason: "Above that, people stop reaching
// for it and the archive dies."
//
// What is measured is a *cold* query, which is what a user pays: a new process,
// an unwarmed page cache in this process, open the database, run the migration
// check, plan, match, rank, snippet, print.

const SEARCH_BUDGET_MS: f64 = 100.0;
const ARCHIVE_SIZE: usize = 100_000;

/// A synthetic archive of `ARCHIVE_SIZE` exchanges, written as transcripts and
/// ingested through the real parser — not injected into SQLite behind its back,
/// because the index is maintained by triggers on the write path and a fixture
/// that skipped it would measure a different program.
fn generate(e: &Env) -> std::path::PathBuf {
    const PER_FILE: usize = 500;
    // Under the temp home, not a plausible-looking `/home/dev`: `resolve_repo`
    // walks to the filesystem root looking for `.git`, and on macOS a stat into
    // a non-existent `/home/...` goes through the automounter. That measured
    // the automounter, not the ingest.
    let root = e.home().join("src");
    // Vocabulary chosen so terms have realistically skewed frequencies: a term
    // that appears in every row measures nothing, and one that appears in a
    // single row measures a lookup rather than a ranking.
    let topics = [
        "postgres migration lock",
        "ffmpeg concat demuxer",
        "kubernetes ingress tls",
        "rust lifetime borrow",
        "sqlite wal checkpoint",
        "docker layer cache",
        "terraform state drift",
        "redis eviction policy",
        "webpack chunk splitting",
        "grpc deadline propagation",
    ];
    let mut n = 0usize;
    for f in 0..ARCHIVE_SIZE / PER_FILE {
        let mut body = String::with_capacity(PER_FILE * 900);
        for _ in 0..PER_FILE {
            let topic = topics[n % topics.len()];
            let ts = format!(
                "2026-{:02}-{:02}T{:02}:{:02}:00.000Z",
                1 + (n % 12),
                1 + (n % 28),
                n % 24,
                n % 60
            );
            body.push_str(&format!(
                r#"{{"type":"user","uuid":"u{n}","parentUuid":null,"sessionId":"s{f}","timestamp":"{ts}","cwd":"{}","gitBranch":"main","message":{{"role":"user","content":"how do I fix the {topic} problem in exchange {n}"}}}}
{{"type":"assistant","uuid":"a{n}","parentUuid":"u{n}","sessionId":"s{f}","timestamp":"{ts}","cwd":"{}","gitBranch":"main","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"The {topic} behaviour is usually a configuration problem. {}"}},{{"type":"tool_use","id":"t{n}","name":"Bash","input":{{"command":"grep -rn {topic} ."}}}}]}}}}
"#,
                root.join(format!("proj{}", n % 20)).display(),
                root.join(format!("proj{}", n % 20)).display(),
                "Here is a paragraph of explanation that makes the response a realistic length rather than a single line. ".repeat(4),
            ));
            n += 1;
        }
        std::fs::write(e.projects().join(format!("proj/gen{f}.jsonl")), body).unwrap();
    }
    e.cmd()
        .args(["capture", "--all", "--quiet"])
        .timeout(std::time::Duration::from_secs(1800))
        .assert()
        .success();
    root
}

#[test]
fn search_p95_is_under_a_hundred_milliseconds_at_100k_exchanges() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    let built = Instant::now();
    let root = generate(&e);
    let rows: i64 = {
        let conn = rusqlite::Connection::open(e.db()).unwrap();
        conn.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        rows, ARCHIVE_SIZE as i64,
        "the archive under measurement must be the size claimed"
    );
    println!(
        "\narchive: {rows} exchanges, {:.1} MB, built in {:.0}s",
        std::fs::metadata(e.db()).unwrap().len() as f64 / 1e6,
        built.elapsed().as_secs_f64()
    );

    // Three query shapes, because they exercise different work: a common term
    // ranks tens of thousands of candidates, a rare one ranks few, and the
    // filtered form is the one scenario 2 leans on.
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("common two-term", vec!["postgres", "migration"]),
        ("rare term", vec!["87421"]),
    ];
    let proj3 = root.join("proj3");
    let filtered = vec![
        "migration",
        "--in",
        proj3.to_str().unwrap(),
        "--since",
        "2026-06-01",
    ];
    let cases: Vec<(&str, Vec<&str>)> = cases
        .into_iter()
        .chain(std::iter::once(("filtered", filtered)))
        .collect();

    let mut worst: f64 = 0.0;
    for (name, args) in &cases {
        let mut samples = Vec::with_capacity(30);
        for i in 0..35 {
            let t = Instant::now();
            let _ = e.cmd().args(args).assert();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if i >= 5 {
                samples.push(ms); // discard warm-up
            }
        }
        samples.sort_by(f64::total_cmp);
        let p50 = percentile(&samples, 0.50);
        let p95 = percentile(&samples, 0.95);
        println!(
            "  {name:<16} p50 {p50:6.2} ms   p95 {p95:6.2} ms   max {:6.2} ms",
            samples[samples.len() - 1]
        );
        worst = worst.max(p95);
    }
    println!();

    assert!(
        worst < SEARCH_BUDGET_MS,
        "p95 {worst:.2} ms exceeds the {SEARCH_BUDGET_MS} ms budget"
    );
}
