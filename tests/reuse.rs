//! Phase 4's Exit criterion: **"a new session answers from a past exchange, and
//! the user can see exactly which one and why it was chosen."**
//!
//! Two halves, and both are tested here. *Reaching* the exchange is the three
//! agent paths — MCP, `tools`/`call`, and `render` on a pipe — and each is
//! driven end to end against the real binary. *Seeing which one and why* is the
//! `cite` field, the `why` field, the attribution in the prompt block, and the
//! stderr line the recall hook prints into the user's own terminal.
//!
//! The rules from docs/plan.md that these exist to hold down: agents read and
//! never write, automatic recall is off until asked for, the caps are caps, and
//! a forgotten exchange is not reachable by any of it.

mod common;

use common::Env;
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// One prompt/response pair in Claude Code's shape.
fn exchange(
    n: usize,
    ts: &str,
    cwd: &str,
    prompt: &str,
    response: &str,
    cmd: Option<&str>,
) -> String {
    let tool = match cmd {
        Some(c) => format!(
            r#",{{"type":"tool_use","id":"t{n}","name":"Bash","input":{{"command":{}}}}}"#,
            serde_json::to_string(c).unwrap()
        ),
        None => String::new(),
    };
    format!(
        r#"{{"type":"user","uuid":"u{n}","parentUuid":null,"sessionId":"s{n}","timestamp":"{ts}","cwd":"{cwd}","gitBranch":"main","message":{{"role":"user","content":{}}}}}
{{"type":"assistant","uuid":"a{n}","parentUuid":"u{n}","sessionId":"s{n}","timestamp":"{ts}","cwd":"{cwd}","gitBranch":"main","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":{}}}{tool}]}}}}
"#,
        serde_json::to_string(prompt).unwrap(),
        serde_json::to_string(response).unwrap(),
    )
}

/// Scenario 3's archive: the clock-skew diagnosis Dana wants back three months
/// later, plus enough neighbours for ranking to have something to do.
fn archive(e: &Env) {
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.write_transcript(
        "webhooks.jsonl",
        &exchange(
            1,
            "2026-03-05T14:00:00.000Z",
            "/home/dev/src/webhooks",
            "the webhook handler rejects every request, signature validation keeps failing",
            "The clock skew tolerance on signature validation is set to zero, so a request \
             that takes even a second to arrive is outside the window. Set a tolerance of \
             300 seconds and compare against both the current and previous window.",
            Some("curl -sS -X POST http://localhost:8080/hooks -d @sample.json"),
        ),
    );
    e.write_transcript(
        "pycon.jsonl",
        &exchange(
            2,
            "2026-03-03T14:22:07.000Z",
            "/home/dev/talks/pycon-2026",
            "I have 4 mp4 files I need to join into one, same codec",
            "Use the concat demuxer with -c copy so nothing is re-encoded.",
            Some("ffmpeg -f concat -safe 0 -i files.txt -c copy out.mp4"),
        ),
    );
    e.write_transcript(
        "billing.jsonl",
        &exchange(
            3,
            "2026-01-19T09:41:55.000Z",
            "/home/dev/src/billing-api",
            "should I backfill tenant_id in one transaction or batch it",
            "Batch it, with a checkpoint table. One transaction holds a lock for the whole \
             backfill and the replicas fall behind.",
            None,
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();
}

// ── The MCP server ───────────────────────────────────────────────────────

/// Drive `tmem mcp` as a client would: newline-delimited JSON-RPC on stdio.
fn mcp(e: &Env, requests: &[&str]) -> Vec<Value> {
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("tmem"))
        .arg("mcp")
        .env("TMEM_HOME", e.home().join("data"))
        .env("TMEM_CLAUDE_PROJECTS", e.projects())
        .env("TMEM_CLAUDE_SETTINGS", e.settings())
        .env("TMEM_CONFIG_DIR", e.home().join("no-config"))
        .env("HOME", e.home())
        .env_remove("TMEM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    } // dropping stdin closes it, which is how the server is asked to stop
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "server exited {:?}", out.status);
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// The Exit criterion over MCP: a session that never saw March gets the March
/// exchange back, with the command that shows the user the same one.
#[test]
fn an_agent_reaches_a_past_exchange_over_mcp_and_can_cite_it() {
    let e = Env::new();
    archive(&e);

    let out = mcp(
        &e,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_memory","arguments":{"query":"signature validation clock skew"}}}"#,
        ],
    );
    // Four requests, one of them a notification: exactly three responses.
    assert_eq!(out.len(), 3, "{out:#?}");
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[1]["result"]["tools"].as_array().unwrap().len(), 3);

    let res = &out[2]["result"];
    assert_eq!(res["isError"], false);
    let hit = &res["structuredContent"]["results"][0];
    let id = hit["id"].as_str().unwrap();
    assert!(
        hit["snippet"].as_str().unwrap().contains("clock skew")
            || hit["prompt"]
                .as_str()
                .unwrap()
                .contains("signature validation"),
        "{hit:#?}"
    );

    // "the user can see exactly which one and why it was chosen"
    assert_eq!(hit["cite"], format!("tmem show {id}"));
    assert!(
        hit["why"].as_str().unwrap().contains("keyword match"),
        "{hit:#?}"
    );
    assert!(!hit["matched"].as_array().unwrap().is_empty());
    assert!(
        hit["ts"].as_str().unwrap().starts_with("2026-03-05T"),
        "{hit:#?}"
    );
    assert_eq!(hit["cwd"], "/home/dev/src/webhooks");

    // And the cited command is one the user can actually run.
    e.cmd().args(["show", id]).assert().success();
}

/// Read-only is a property of the handle, not a promise. The MCP server opens
/// the archive with `SQLITE_OPEN_READ_ONLY`, so nothing reached through it can
/// change a byte — which this checks the blunt way, by hashing the file.
#[test]
fn nothing_an_agent_can_ask_for_changes_the_archive() {
    let e = Env::new();
    archive(&e);
    let before = std::fs::read(e.db()).unwrap();

    mcp(
        &e,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_memory","arguments":{"query":"backfill"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"recent","arguments":{}}}"#,
            // The tools a hopeful model might try anyway.
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"forget","arguments":{"id":"01"}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"write_memory","arguments":{}}}"#,
        ],
    );
    assert_eq!(
        before,
        std::fs::read(e.db()).unwrap(),
        "the archive changed"
    );
    // And the count is unchanged, in case the file were rewritten identically.
    assert_eq!(e.count("exchanges"), 3);
}

/// A model that misspells an argument must be told, not answered. Silently
/// widening `{"querry": …}` into "everything" hands it a false picture of the
/// user's history that neither it nor the user can notice.
#[test]
fn a_wrong_argument_reaches_the_model_as_an_error_it_can_fix() {
    let e = Env::new();
    archive(&e);
    let out = mcp(
        &e,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_memory","arguments":{"querry":"backfill"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_memory","arguments":{}}}"#,
        ],
    );
    for r in &out {
        assert_eq!(r["result"]["isError"], true, "{r:#?}");
        assert!(r["error"].is_null(), "a tool error is not a protocol error");
    }
    let text = out[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unknown argument 'querry'"), "{text}");
}

/// `forget` is irreversible and the agent surface is not a way around it.
#[test]
fn a_forgotten_exchange_is_not_reachable_by_any_agent_path() {
    let e = Env::new();
    archive(&e);
    let id = e
        .query("SELECT id FROM exchanges WHERE cwd LIKE '%webhooks%'")
        .remove(0);
    e.cmd().args(["forget", &id, "-y"]).assert().success();

    let get = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"get_exchange","arguments":{{"id":"{id}"}}}}}}"#
    );
    let out = mcp(
        &e,
        &[
            &get,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_memory","arguments":{"query":"signature validation clock skew"}}}"#,
        ],
    );
    assert_eq!(
        out[0]["result"]["structuredContent"]["count"], 0,
        "{out:#?}"
    );
    let whole = out[1].to_string();
    assert!(!whole.contains("clock skew"), "{whole}");
    assert!(!whole.contains(&id), "{whole}");
}

// ── tools / call ─────────────────────────────────────────────────────────

#[test]
fn the_openai_schema_is_the_same_three_tools_and_is_valid_json() {
    let e = Env::new();
    archive(&e);
    let out = e
        .cmd()
        .args(["tools", "--schema", "openai"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["search_memory", "get_exchange", "recent"]);
    assert_eq!(v[0]["type"], "function");
    assert_eq!(v[0]["function"]["parameters"]["required"][0], "query");

    // …and the MCP envelope of the same thing, for a client that wants it.
    let out = e.cmd().args(["tools", "--schema", "mcp"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v[0]["inputSchema"].is_object());

    e.cmd()
        .args(["tools", "--schema", "nonsense"])
        .assert()
        .code(2);
}

/// The same three tools without a protocol, which is what a local model behind
/// Ollama or llama.cpp actually gets.
#[test]
fn call_runs_a_tool_and_carries_the_documented_exit_codes() {
    let e = Env::new();
    archive(&e);

    let out = e
        .cmd()
        .args([
            "call",
            "search_memory",
            "--args",
            r#"{"query":"concat demuxer"}"#,
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "found is exit 0");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["count"].as_u64().unwrap() >= 1);
    assert!(v["results"][0]["cite"]
        .as_str()
        .unwrap()
        .starts_with("tmem show "));

    // docs/cli.md: 1 is "nothing found", 2 is "error". The JSON is printed
    // either way, so a caller can read the result *and* branch on the code.
    let out = e
        .cmd()
        .args([
            "call",
            "search_memory",
            "--args",
            r#"{"query":"zzzznothing"}"#,
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["count"], 0);
    assert!(v["note"].as_str().unwrap().contains("Nothing matched"));

    e.cmd()
        .args(["call", "search_memory", "--args", "{not json}"])
        .assert()
        .code(2);
    e.cmd().args(["call", "delete_everything"]).assert().code(2);

    // Arguments on stdin, because a model-generated call is quote-hostile.
    let out = e
        .cmd()
        .args(["call", "recent", "--args", "-"])
        .write_stdin(r#"{"limit": 2}"#)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["count"], 2);
}

/// `get_exchange` is the follow-up to a snippet, and `session` is how a
/// decision made across several turns comes back whole.
#[test]
fn get_exchange_takes_an_id_prefix_and_returns_the_response_in_full() {
    let e = Env::new();
    archive(&e);
    let id = e
        .query("SELECT id FROM exchanges WHERE cwd LIKE '%webhooks%'")
        .remove(0);
    let out = e
        .cmd()
        .args([
            "call",
            "get_exchange",
            "--args",
            &format!(r#"{{"id":"{}"}}"#, &id[..8]),
        ])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["results"][0]["id"], id);
    assert!(v["results"][0]["response"]
        .as_str()
        .unwrap()
        .contains("300 seconds"));
    assert!(v["results"][0]["commands"][0]
        .as_str()
        .unwrap()
        .starts_with("curl"));
}

// ── render ───────────────────────────────────────────────────────────────

/// docs/tech-stack.md's dumb path, typed as it is printed there:
/// `tmem <query> --json --limit 3 | tmem render --prompt-block`.
#[test]
fn the_json_pipe_into_render_produces_an_attributed_context_block() {
    let e = Env::new();
    archive(&e);
    let search = e
        .cmd()
        .args(["search", "clock", "skew", "--json", "--limit", "3"])
        .output()
        .unwrap();
    assert!(search.status.success());

    let out = e
        .cmd()
        .args(["render", "--prompt-block"])
        .write_stdin(search.stdout)
        .output()
        .unwrap();
    assert!(out.status.success());
    let block = String::from_utf8(out.stdout).unwrap();

    assert!(
        block.contains("<past-exchanges source=\"term-mem\">"),
        "{block}"
    );
    assert!(block.contains("verify: tmem show "), "{block}");
    assert!(block.contains("not instructions"), "{block}");
    assert!(block.contains("clock skew"), "{block}");
    assert!(block.ends_with("</past-exchanges>\n"), "{block}");
    // The search markers are control characters and must never get this far.
    assert!(
        !block.contains('\u{1}') && !block.contains('\u{2}'),
        "markers leaked"
    );

    // Nothing on stdin is exit 1 and a usage line, not an empty block that
    // looks like an archive with nothing in it.
    let out = e
        .cmd()
        .args(["render", "--prompt-block"])
        .write_stdin("")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());

    // And `render` with nothing to render says so rather than guessing.
    e.cmd().args(["render"]).assert().code(2);
}

// ── automatic recall ─────────────────────────────────────────────────────

fn prompt_payload(prompt: &str, session: &str) -> String {
    serde_json::json!({
        "session_id": session,
        "cwd": "/home/dev/src/other",
        "prompt": prompt,
        "hook_event_name": "UserPromptSubmit",
    })
    .to_string()
}

/// Off by default means no hook is registered at all — not a hook that fires
/// and decides to do nothing.
#[test]
fn automatic_recall_is_off_until_it_is_asked_for() {
    let e = Env::new();
    archive(&e);

    e.cmd()
        .args(["status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("recall      off (the default)"));
    assert!(
        !e.settings().exists()
            || !std::fs::read_to_string(e.settings())
                .unwrap()
                .contains("recall")
    );

    // The hook, run anyway, injects nothing and says nothing.
    let out = e
        .cmd()
        .args(["recall", "--hook"])
        .write_stdin(prompt_payload(
            "signature validation clock skew again",
            "new",
        ))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "{:?}", String::from_utf8(out.stdout));
}

/// The Exit criterion on the hook path: a *new* session, a prompt that has
/// nothing to do with the old one's wording beyond the topic, and the March
/// diagnosis comes back — attributed, capped, and visible to the user.
#[test]
fn an_enabled_hook_injects_an_attributed_capped_block_and_tells_the_user() {
    let e = Env::new();
    archive(&e);
    // A user's own unrelated hook, which must survive being edited around.
    std::fs::write(
        e.settings(),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"mine.sh"}]}]},"model":"opus"}"#,
    )
    .unwrap();

    e.cmd().args(["recall", "--enable"]).assert().success();
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(e.settings()).unwrap()).unwrap();
    assert_eq!(settings["model"], "opus", "the rest of the file survived");
    let text = settings["hooks"]["UserPromptSubmit"].to_string();
    assert!(text.contains("tmem recall --hook"), "{text}");
    assert!(
        text.contains("mine.sh"),
        "the user's own hook survived: {text}"
    );

    let out = e
        .cmd()
        .args(["recall", "--hook"])
        .write_stdin(prompt_payload(
            "signature validation is failing again on a different service, clock skew?",
            "a-brand-new-session",
        ))
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let block = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(block.contains("clock skew"), "{block}");
    assert!(block.contains("source=\"term-mem\""), "{block}");
    assert!(block.contains("verify: tmem show "), "{block}");

    // docs/plan.md: capped at 3 exchanges and ~1500 tokens.
    assert!(block.matches("verify: tmem show ").count() <= 3, "{block}");
    assert!(block.len() <= 1500 * 4, "{} chars", block.len());

    // "always visibly attributed" — the user sees it in their own terminal,
    // not only inside the model's context.
    assert!(stderr.contains("tmem: recalled"), "{stderr}");
    assert!(stderr.contains("tmem show "), "{stderr}");

    // And the id it names resolves.
    let id = stderr
        .split("tmem show ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    e.cmd().args(["show", id]).assert().success();
}

/// Telling a model what it just said, at 1500 tokens a turn, is the cheapest
/// way to make this feature feel broken.
#[test]
fn recall_never_replays_the_session_that_is_asking() {
    let e = Env::new();
    archive(&e);
    e.cmd().args(["recall", "--enable"]).assert().success();
    let session = e
        .query("SELECT session_id FROM exchanges WHERE cwd LIKE '%webhooks%'")
        .remove(0);

    let out = e
        .cmd()
        .args(["recall", "--hook"])
        .write_stdin(prompt_payload(
            "signature validation clock skew tolerance",
            &session,
        ))
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !stdout.contains("clock skew"),
        "replayed the asking session's own exchange: {stdout}"
    );
}

/// A prompt with nothing behind it in the archive gets nothing — the failure
/// this feature has to avoid is recalling *something* for every prompt.
#[test]
fn an_unrelated_prompt_recalls_nothing() {
    let e = Env::new();
    archive(&e);
    e.cmd().args(["recall", "--enable"]).assert().success();
    for prompt in [
        "what is the capital of Peru",
        "hi",
        "refactor the parser to use a visitor",
    ] {
        let out = e
            .cmd()
            .args(["recall", "--hook"])
            .write_stdin(prompt_payload(prompt, "s-new"))
            .output()
            .unwrap();
        assert!(
            String::from_utf8(out.stdout).unwrap().trim().is_empty(),
            "injected context for '{prompt}'"
        );
    }
}

/// `tmem recall <words>` is how a user checks what the hook would do without
/// starting a session — the inspectable half of "and why it was chosen".
#[test]
fn the_preview_shows_what_would_be_injected_and_why() {
    let e = Env::new();
    archive(&e);
    let out = e
        .cmd()
        .args(["recall", "signature", "validation", "clock", "skew"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("would be prepended"), "{stderr}");
    assert!(stderr.contains("score "), "the score is shown: {stderr}");
    assert!(stderr.contains("query term(s) present"), "{stderr}");
    assert!(stderr.contains("automatic recall is off"), "{stderr}");
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("clock skew"));

    let out = e.cmd().args(["recall", "zzzznothing"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("nothing clears the bar"));
}

/// Disabling puts the user's settings.json back the way it was, and `doctor`
/// refuses to let the two halves disagree quietly.
#[test]
fn disable_removes_the_hook_and_doctor_catches_a_half_disabled_state() {
    let e = Env::new();
    archive(&e);
    std::fs::write(
        e.settings(),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"mine.sh"}]}]}}"#,
    )
    .unwrap();
    let before = std::fs::read_to_string(e.settings()).unwrap();

    e.cmd().args(["recall", "--enable"]).assert().success();
    e.cmd()
        .args(["doctor"])
        .assert()
        .stdout(predicates::str::contains("automatic recall is on"));

    e.cmd().args(["recall", "--disable"]).assert().success();
    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(e.settings()).unwrap()).unwrap();
    let before: Value = serde_json::from_str(&before).unwrap();
    assert_eq!(before, after, "settings.json did not come back clean");

    // Now break it by hand, the way a user editing settings.json would.
    e.cmd().args(["recall", "--enable"]).assert().success();
    let mut root: Value =
        serde_json::from_str(&std::fs::read_to_string(e.settings()).unwrap()).unwrap();
    root["hooks"]["UserPromptSubmit"] = serde_json::json!([]);
    std::fs::write(e.settings(), root.to_string()).unwrap();
    e.cmd()
        .args(["doctor"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains(
            "no UserPromptSubmit hook is registered",
        ));
    e.cmd()
        .args(["status"])
        .assert()
        .stdout(predicates::str::contains("INCONSISTENT"));
}

/// A settings file that has been hand-edited into a shape we did not write
/// must produce an error, never a silently unregistered hook.
#[test]
fn a_settings_file_we_cannot_understand_is_an_error_not_a_shrug() {
    let e = Env::new();
    archive(&e);
    std::fs::write(
        e.settings(),
        r#"{"hooks":{"UserPromptSubmit":"not-an-array"}}"#,
    )
    .unwrap();
    e.cmd().args(["recall", "--enable"]).assert().code(2);
}

/// docs/plan.md's note on this phase: redacted rows go to the agent "as they
/// are stored" — and flagged, so the model can tell term-mem's placeholder
/// from something the user wrote.
#[test]
fn a_redacted_exchange_reaches_the_agent_as_stored_and_says_so() {
    let e = Env::new();
    e.cmd().args(["init", "--no-hook"]).assert().success();
    e.write_transcript(
        "leak.jsonl",
        &exchange(
            9,
            "2026-03-06T10:00:00.000Z",
            "/home/dev/src/webhooks",
            "the staging webhook returns 401, here is the request log",
            "Authorization: Bearer sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
             — that token is what the service rejected; the clock skew is a separate problem.",
            None,
        ),
    );
    e.cmd().args(["capture", "--all"]).assert().success();
    assert_eq!(e.count("exchanges"), 1);

    let out = e
        .cmd()
        .args([
            "call",
            "search_memory",
            "--args",
            r#"{"query":"webhook staging 401"}"#,
        ])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let whole = v.to_string();
    assert!(
        !whole.contains("sk-ant-api03-AAAA"),
        "the token reached an agent: {whole}"
    );
    assert_eq!(v["results"][0]["redacted"], true, "{v:#?}");
    assert!(v["results"][0]["redacted_note"]
        .as_str()
        .unwrap()
        .contains("term-mem's"));
}
