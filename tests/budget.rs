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
