//! `tmem mcp` — Model Context Protocol over stdio.
//!
//! Hand-rolled JSON-RPC rather than an MCP SDK, and deliberately. docs/mission.md
//! is one line — *nothing leaves the machine* — and the maintained Rust MCP
//! crates pull in an async runtime and HTTP/SSE transports whose presence we
//! would then have to argue about on every audit. The stdio transport is
//! newline-delimited JSON-RPC 2.0, which `serde_json` already speaks. The whole
//! protocol surface this server needs is below, in one screen, with no socket
//! anywhere in it.
//!
//! The server is **read-only by construction**: it opens the archive with
//! `SQLITE_OPEN_READ_ONLY`, so "agents never write" is a property of the file
//! handle rather than a promise in a doc comment.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// The revision of MCP this server implements. A client that asks for a
/// different one is answered with ours: the spec's negotiation is "the server
/// states what it supports", and quietly echoing back a version we do not
/// implement would be the lie this project is built not to tell.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Versions whose stdio surface this server is compatible with, echoed back
/// when a client asks for one of them.
const ALSO_SPOKEN: &[&str] = &["2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

pub fn run() -> Result<i32> {
    let conn = crate::db::open_readonly(&crate::paths::db_path()?)?;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    serve(&conn, &mut stdin.lock(), &mut out)?;
    Ok(crate::output::EXIT_OK)
}

/// The loop, split from `run` so tests can drive it over a pair of buffers
/// instead of a process.
pub fn serve<R: BufRead, W: Write>(conn: &Connection, input: &mut R, out: &mut W) -> Result<()> {
    // Bytes, not `String`. `read_line` on a `String` fails the whole call on
    // invalid UTF-8, which took the server down on exactly the input it is
    // supposed to answer with -32700 — the malformed-JSON test passed because
    // malformed JSON is still valid UTF-8, and a truncated multi-byte character
    // from a crashing client is not.
    let mut raw: Vec<u8> = Vec::new();
    loop {
        raw.clear();
        if input.read_until(b'\n', &mut raw)? == 0 {
            return Ok(()); // EOF: the client went away, which is how this ends.
        }
        let line = String::from_utf8_lossy(&raw);
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle_line(conn, &line) else {
            continue; // a notification; JSON-RPC forbids a reply
        };
        // One message per line, flushed immediately: a client blocked waiting
        // for a response that is sitting in our buffer looks like a hang.
        //
        // A write error ends the loop rather than propagating: it means the
        // client closed its end, which is the same event as EOF and is not a
        // failure of this process.
        if writeln!(out, "{response}")
            .and_then(|()| out.flush())
            .is_err()
        {
            return Ok(());
        }
    }
}

fn handle_line(conn: &Connection, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error(
                Value::Null,
                PARSE_ERROR,
                &format!("invalid JSON: {e}"),
            ))
        }
    };
    // Batches are legal JSON-RPC and are not part of the MCP stdio surface any
    // client uses. Refusing one loudly beats half-handling it.
    let Some(obj) = msg.as_object() else {
        return Some(error(
            Value::Null,
            INVALID_REQUEST,
            "expected a single JSON-RPC request object",
        ));
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = obj.get("id").is_none();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return if is_notification {
            None
        } else {
            Some(error(id, INVALID_REQUEST, "request has no 'method'"))
        };
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);

    let outcome = route(conn, method, &params);
    if is_notification {
        return None; // e.g. notifications/initialized
    }
    Some(match outcome {
        Route::Result(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
        Route::Error(code, msg) => error(id, code, &msg),
    })
}

enum Route {
    Result(Value),
    Error(i64, String),
}

fn route(conn: &Connection, method: &str, params: &Value) -> Route {
    match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = if ALSO_SPOKEN.contains(&asked) {
                asked
            } else {
                PROTOCOL_VERSION
            };
            Route::Result(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "term-mem",
                    "title": "term-mem — the user's local conversation archive",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions":
                    "term-mem is a local, read-only archive of this user's own past terminal \
                     AI conversations. Search it when the user refers to something they did \
                     before — a command that worked, a decision that was made — rather than \
                     guessing or asking them to remember. Every result carries a `cite` field \
                     holding the exact command that shows the user the same exchange; quote it \
                     when you rely on one. You cannot write to or delete from this archive.",
            }))
        }
        "ping" => Route::Result(json!({})),
        "tools/list" => match crate::mcp::mcp_schema() {
            Ok(tools) => Route::Result(json!({ "tools": tools })),
            Err(e) => Route::Error(-32603, format!("{e:#}")),
        },
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return Route::Error(INVALID_REQUEST, "tools/call: 'name' is required".into());
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            // A tool that fails is a *result* with `isError`, not a protocol
            // error — the model is supposed to see the message and correct
            // itself, which it cannot do if the failure never reaches it.
            match crate::mcp::dispatch(conn, name, &args) {
                Ok(v) => Route::Result(json!({
                    "content": [{ "type": "text", "text": pretty(&v) }],
                    "structuredContent": v,
                    "isError": false,
                })),
                Err(e) => Route::Result(json!({
                    "content": [{ "type": "text", "text": format!("term-mem: {e:#}") }],
                    "isError": true,
                })),
            }
        }
        other => Route::Error(METHOD_NOT_FOUND, format!("unsupported method '{other}'")),
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE exchanges (id TEXT PRIMARY KEY, assistant TEXT, session_id TEXT, \
             thread_id TEXT, source_key TEXT, ts INTEGER, cwd TEXT, repo TEXT, git_branch TEXT, \
             model TEXT, prompt TEXT, response TEXT, redacted INTEGER DEFAULT 0);
             CREATE TABLE commands (exchange_id TEXT, seq INTEGER, cmd TEXT, lang TEXT);
             CREATE TABLE file_refs (exchange_id TEXT, seq INTEGER, path TEXT, tool TEXT);
             INSERT INTO exchanges VALUES ('01ABC','claude-code','s','t','k',0,'/home/dev',\
             NULL,NULL,NULL,'how do I concat mp4','use the concat demuxer',0);",
        )
        .unwrap();
        c
    }

    fn call(c: &Connection, line: &str) -> Option<Value> {
        handle_line(c, line)
    }

    #[test]
    fn initialize_then_list_then_call() {
        let c = conn();
        let init = call(
            &c,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(init["result"]["serverInfo"]["name"], "term-mem");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        let list = call(&c, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 3);

        let got = call(
            &c,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"recent","arguments":{}}}"#,
        )
        .unwrap();
        assert_eq!(got["result"]["isError"], false);
        assert_eq!(got["result"]["structuredContent"]["count"], 1);
        let text = got["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("01ABC"), "{text}");
        assert!(text.contains("tmem show 01ABC"), "provenance: {text}");
    }

    /// An unknown protocol version must not be echoed back as if we spoke it.
    #[test]
    fn an_unknown_protocol_version_gets_ours_not_its_own_back() {
        let c = conn();
        let r = call(
            &c,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#,
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        let r = call(
            &c,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
    }

    /// A notification has no id and must produce no reply at all. A stray
    /// response to `notifications/initialized` desynchronises a strict client.
    #[test]
    fn notifications_are_never_answered() {
        let c = conn();
        assert!(call(
            &c,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
        assert!(call(
            &c,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#
        )
        .is_none());
        // …but a *request* for an unsupported method is answered, with -32601.
        let r = call(&c, r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#).unwrap();
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    }

    /// Bytes that are not UTF-8 are the case a `String`-based read loop dies
    /// on, and a crashing client that writes half a character is exactly how
    /// they arrive.
    #[test]
    fn invalid_utf8_does_not_take_the_server_down() {
        let c = conn();
        let mut input = std::io::Cursor::new(
            [
                b"\xff\xfe not json\n".to_vec(),
                br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec(),
                b"\n".to_vec(),
            ]
            .concat(),
        );
        let mut out: Vec<u8> = Vec::new();
        serve(&c, &mut input, &mut out).expect("the loop survived");
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("-32700"), "{}", lines[0]);
        assert!(lines[1].contains(r#""result":{}"#), "{}", lines[1]);
    }

    /// Garbage on stdin must not take the server down: the client is another
    /// program and the session should survive its bugs.
    #[test]
    fn malformed_input_is_an_error_response_not_a_crash() {
        let c = conn();
        let r = call(&c, "{not json").unwrap();
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        assert_eq!(r["id"], Value::Null);
        let r = call(&c, "[1,2,3]").unwrap();
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        let r = call(&c, r#"{"jsonrpc":"2.0","id":4}"#).unwrap();
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        // And the loop keeps going afterwards.
        let mut input = std::io::Cursor::new(
            "{bad\n\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_string(),
        );
        let mut out: Vec<u8> = Vec::new();
        serve(&c, &mut input, &mut out).unwrap();
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains(r#""result":{}"#), "{}", lines[1]);
    }

    /// A tool error reaches the model as a result it can read and retry from,
    /// not as a JSON-RPC error it never sees.
    #[test]
    fn a_bad_tool_call_is_a_result_with_is_error() {
        let c = conn();
        let r = call(
            &c,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_memory","arguments":{"querry":"x"}}}"#,
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["error"].is_null());
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("unknown argument 'querry'"), "{text}");
    }

    /// There is no tool that changes anything, by any name.
    #[test]
    fn there_is_no_write_tool_on_the_wire() {
        let c = conn();
        let list = call(&c, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let text = list.to_string();
        for forbidden in ["forget", "delete", "import", "capture", "write", "pause"] {
            assert!(
                !text.contains(&format!("\"name\":\"{forbidden}")),
                "{forbidden} is exposed"
            );
        }
    }
}
