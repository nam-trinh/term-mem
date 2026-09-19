//! The agent-facing surface: three read-only tools over the archive, defined
//! once and reached three ways.
//!
//! docs/tech-stack.md names `search_memory`, `get_exchange` and `recent`, and
//! puts two rules on them. **Read-only** — an agent searches the archive and
//! never writes to or deletes from it, which is enforced here by opening the
//! database with `SQLITE_OPEN_READ_ONLY` rather than by convention. And
//! **results carry provenance**, so the agent can cite where a claim came from
//! and the user can go and read it.
//!
//! One definition serves the MCP server (`tmem mcp`), the OpenAI-shaped schema
//! dump (`tmem tools --schema openai`), and the direct executor (`tmem call`).
//! Three envelopes, one implementation — a second copy would drift, and an
//! agent told the wrong thing about the archive is the failure mode this whole
//! project exists to avoid.

pub mod server;

use crate::cli::{in_path_candidates, timespec};
use crate::db::queries::{self, Exchange, Filter};
use crate::output::{fmt_rfc3339, tilde};
use crate::search::{self, Hit};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Map, Value};

/// A tool result never carries more than this many exchanges, whatever the
/// caller asks for. An agent that requests 10,000 rows does not get a better
/// answer, it gets a context window full of someone else's afternoon.
const MAX_LIMIT: usize = 50;
const DEFAULT_LIMIT: usize = 10;

/// Prompts are usually a paragraph but occasionally a pasted stack trace. A
/// ranked *list* truncates them; `get_exchange` is the way to see one in full,
/// and every result says so in its `cite` field.
const LIST_PROMPT_CHARS: usize = 400;
const LIST_COMMANDS: usize = 10;

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "search_memory",
        description: "Search the user's own archive of past terminal AI conversations by \
                      keyword. Ranked with BM25, with command lines weighted above prose. \
                      Returns snippets and provenance, not whole transcripts — follow up with \
                      get_exchange for the full text. Terms are stemmed and OR-ed, so a bare \
                      phrase works; there is no query syntax to learn.",
    },
    ToolDef {
        name: "get_exchange",
        description: "Fetch one past exchange in full by id (any unambiguous id prefix works), \
                      or with session=true the whole conversation thread around it. Use after \
                      search_memory when a snippet looks relevant and you need the reasoning.",
    },
    ToolDef {
        name: "recent",
        description: "The user's latest exchanges, newest first, optionally limited to a \
                      directory tree. The backstop for when keyword search finds nothing, \
                      which happens whenever the user cannot reconstruct the wording.",
    },
];

/// JSON Schema for one tool's arguments. Shared verbatim by MCP's `inputSchema`
/// and by the `parameters` of an OpenAI function definition — they are the same
/// dialect, which is the only reason one surface can serve both.
pub fn schema(name: &str) -> Result<Value> {
    Ok(match name {
        "search_memory" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string",
                    "description": "Words to search for. Stemmed and OR-ed; no syntax." },
                "in": { "type": "string",
                    "description": "Absolute path. Limit to exchanges recorded in this \
                                    directory tree." },
                "since": { "type": "string",
                    "description": "Limit by time: `2h`, `7d`, `3w`, `2 hours ago`, `today`, \
                                    `yesterday`, `january`, or `2026-03-01`." },
                "repo": { "type": "string",
                    "description": "Limit to a git repository by name, e.g. `billing-api`." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT,
                    "description": "How many results (default 10)." }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "get_exchange" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string",
                    "description": "Exchange id, or any unambiguous prefix of one." },
                "session": { "type": "boolean",
                    "description": "Return the whole conversation thread around it rather \
                                    than the single exchange (default false)." }
            },
            "required": ["id"],
            "additionalProperties": false
        }),
        "recent" => json!({
            "type": "object",
            "properties": {
                "in": { "type": "string",
                    "description": "Absolute path. Limit to this directory tree." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT,
                    "description": "How many results (default 10)." }
            },
            "required": [],
            "additionalProperties": false
        }),
        other => bail!("unknown tool '{other}'; known tools are {}", tool_names()),
    })
}

pub fn tool_names() -> String {
    TOOLS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

/// The OpenAI tool-calling shape, for a local model behind Ollama, llama.cpp or
/// vLLM. Same three tools, same schemas, different envelope.
pub fn openai_schema() -> Result<Value> {
    let mut out = Vec::new();
    for t in TOOLS {
        out.push(json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": schema(t.name)?,
            }
        }));
    }
    Ok(Value::Array(out))
}

/// The MCP `tools/list` shape.
pub fn mcp_schema() -> Result<Value> {
    let mut out = Vec::new();
    for t in TOOLS {
        out.push(json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": schema(t.name)?,
        }));
    }
    Ok(Value::Array(out))
}

// ---------------------------------------------------------------- arguments

/// Argument extraction that refuses to guess.
///
/// An unknown key is an error rather than something to ignore. A model that
/// calls `search_memory({"querry": "ffmpeg"})` and gets the whole archive back
/// has been told something false about the user's history, and neither it nor
/// the user has any way to notice. docs/CLAUDE.md: silent failure is the enemy.
fn args_of<'a>(args: &'a Value, tool: &str) -> Result<&'a Map<String, Value>> {
    let obj = match args {
        Value::Object(o) => o,
        Value::Null => bail!("{tool}: arguments are required"),
        _ => bail!("{tool}: arguments must be a JSON object"),
    };
    let allowed: Vec<String> = schema(tool)?["properties"]
        .as_object()
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();
    for k in obj.keys() {
        if !allowed.contains(k) {
            bail!(
                "{tool}: unknown argument '{k}'; accepted arguments are {}",
                allowed.join(", ")
            );
        }
    }
    Ok(obj)
}

fn opt_str(o: &Map<String, Value>, k: &str, tool: &str) -> Result<Option<String>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => bail!("{tool}: '{k}' must be a string"),
    }
}

fn opt_bool(o: &Map<String, Value>, k: &str, tool: &str) -> Result<bool> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => bail!("{tool}: '{k}' must be true or false"),
    }
}

/// Clamped rather than rejected: a model asking for 500 results wants "lots",
/// and failing the call teaches it nothing. The clamp is reported back in the
/// result so the answer is not quietly smaller than it looks.
fn opt_limit(o: &Map<String, Value>, tool: &str) -> Result<(usize, bool)> {
    let raw = match o.get("limit") {
        None | Some(Value::Null) => return Ok((DEFAULT_LIMIT, false)),
        Some(Value::Number(n)) => n
            .as_i64()
            .with_context(|| format!("{tool}: 'limit' must be a whole number"))?,
        Some(_) => bail!("{tool}: 'limit' must be a number"),
    };
    if raw < 1 {
        bail!("{tool}: 'limit' must be at least 1");
    }
    let n = raw as usize;
    Ok((n.min(MAX_LIMIT), n > MAX_LIMIT))
}

fn filter_of(o: &Map<String, Value>, tool: &str, limit: usize) -> Result<Filter> {
    Ok(Filter {
        in_paths: opt_str(o, "in", tool)?
            .map(|p| in_path_candidates(std::path::Path::new(&p)))
            .unwrap_or_default(),
        since_ms: opt_str(o, "since", tool)?
            .as_deref()
            .map(timespec::parse)
            .transpose()?,
        repo: opt_str(o, "repo", tool)?,
        limit: Some(limit),
    })
}

// ----------------------------------------------------------------- results

/// One exchange as an agent sees it: the text, and enough provenance to cite it
/// and for the user to go and check.
///
/// `cite` is the load-bearing field. docs/plan.md's Exit criterion for this
/// phase is that "the user can see exactly which one and why it was chosen", so
/// every result carries the literal command that shows it.
fn provenance(ex: &Exchange, full: bool) -> Value {
    let prompt = if full {
        ex.prompt.clone()
    } else {
        truncate(&ex.prompt, LIST_PROMPT_CHARS)
    };
    let mut v = json!({
        "id": ex.id,
        "ts": fmt_rfc3339(ex.ts),
        "cwd": tilde(&ex.cwd),
        "assistant": ex.assistant,
        "thread_id": ex.thread_id,
        "prompt": prompt,
        "cite": format!("tmem show {}", ex.id),
    });
    let o = v.as_object_mut().expect("object");
    if let Some(r) = &ex.repo {
        o.insert("repo".into(), json!(r));
    }
    if let Some(b) = &ex.git_branch {
        o.insert("branch".into(), json!(b));
    }
    if let Some(m) = &ex.model {
        o.insert("model".into(), json!(m));
    }
    // docs/plan.md, Phase 4: anything that reads exchanges must decide what it
    // does about redacted rows, and "show them as they are stored" is the
    // answer. So the placeholder text goes to the agent unchanged — and the
    // flag goes with it, so the agent can tell a `[redacted:…]` marker from
    // something the user actually wrote.
    if ex.redacted {
        o.insert("redacted".into(), json!(true));
        o.insert(
            "redacted_note".into(),
            json!(
                "Parts of this exchange were replaced by term-mem's redactor before it was \
                   stored. `[redacted:<rule>]` markers are term-mem's, not the user's."
            ),
        );
    }
    if !ex.commands.is_empty() {
        let shown: Vec<&String> = ex.commands.iter().take(LIST_COMMANDS).collect();
        o.insert("commands".into(), json!(shown));
        if ex.commands.len() > shown.len() {
            o.insert(
                "commands_omitted".into(),
                json!(ex.commands.len() - shown.len()),
            );
        }
    }
    if full {
        o.insert("response".into(), json!(ex.response));
        o.insert("session_id".into(), json!(ex.session_id));
        if !ex.files.is_empty() {
            o.insert("files".into(), json!(ex.files));
        }
    }
    v
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// A search result: provenance, the matched region, and *why* this row is here.
fn hit_value(h: &Hit) -> Value {
    let matched = crate::search::matched_terms(&h.snippet);
    let mut v = provenance(&h.exchange, false);
    let o = v.as_object_mut().expect("object");
    o.insert(
        "snippet".into(),
        json!(search::strip_markers(&h.snippet).trim().to_string()),
    );
    o.insert("score".into(), json!((h.score * 100.0).round() / 100.0));
    o.insert("matched".into(), json!(matched));
    o.insert("why".into(), json!(why(&matched, h.score)));
    v
}

/// The human-readable half of "why it was chosen". Deliberately says what the
/// ranking *is* rather than implying more: BM25 over a keyword index, with the
/// command column weighted above prose, and nothing else.
fn why(matched: &[String], score: f64) -> String {
    let terms = if matched.is_empty() {
        "the query terms".to_string()
    } else {
        matched.join(", ")
    };
    format!(
        "keyword match on {terms} (BM25 {:.2}, command lines weighted above prose); \
         no semantic search is involved",
        score
    )
}

// ---------------------------------------------------------------- dispatch

/// Run one tool. The single place any of the three envelopes reaches the
/// archive.
pub fn dispatch(conn: &Connection, name: &str, args: &Value) -> Result<Value> {
    match name {
        "search_memory" => search_memory(conn, args),
        "get_exchange" => get_exchange(conn, args),
        "recent" => recent(conn, args),
        other => bail!("unknown tool '{other}'; known tools are {}", tool_names()),
    }
}

fn envelope(tool: &str, results: Vec<Value>, clamped: bool, note: Option<String>) -> Value {
    let mut v = json!({
        "tool": tool,
        "count": results.len(),
        "results": results,
        "source": "term-mem — the user's own local archive, read-only",
    });
    let o = v.as_object_mut().expect("object");
    if clamped {
        o.insert(
            "note".into(),
            json!(format!("'limit' was clamped to {MAX_LIMIT}.")),
        );
    }
    if let Some(n) = note {
        o.insert("note".into(), json!(n));
    }
    v
}

fn search_memory(conn: &Connection, args: &Value) -> Result<Value> {
    const TOOL: &str = "search_memory";
    let o = args_of(args, TOOL)?;
    let query = opt_str(o, "query", TOOL)?
        .context("search_memory: 'query' is required and must be a non-empty string")?;
    let (limit, clamped) = opt_limit(o, TOOL)?;
    let filter = filter_of(o, TOOL, limit)?;
    let terms: Vec<String> = query.split_whitespace().map(str::to_string).collect();
    let hits = search::search(conn, &terms, &filter)?;
    let results: Vec<Value> = hits.iter().map(hit_value).collect();
    let note = if results.is_empty() {
        Some(
            "Nothing matched. Keyword recall fails whenever the user cannot reconstruct the \
             wording; try `recent` with an `in` path instead, or a shorter query."
                .to_string(),
        )
    } else {
        None
    };
    Ok(envelope(TOOL, results, clamped, note))
}

fn get_exchange(conn: &Connection, args: &Value) -> Result<Value> {
    const TOOL: &str = "get_exchange";
    let o = args_of(args, TOOL)?;
    let id = opt_str(o, "id", TOOL)?
        .context("get_exchange: 'id' is required and must be a non-empty string")?;
    let session = opt_bool(o, "session", TOOL)?;
    let Some(resolved) = queries::resolve_id(conn, &id)? else {
        return Ok(envelope(
            TOOL,
            Vec::new(),
            false,
            Some(format!(
                "No exchange with id '{id}'. Ids come from search_memory or recent; the user \
                 may also have deleted it with `tmem forget`, which is permanent."
            )),
        ));
    };
    let rows = if session {
        queries::thread(conn, &resolved)?
    } else {
        queries::get(conn, &resolved)?.into_iter().collect()
    };
    let results: Vec<Value> = rows.iter().map(|e| provenance(e, true)).collect();
    Ok(envelope(TOOL, results, false, None))
}

fn recent(conn: &Connection, args: &Value) -> Result<Value> {
    const TOOL: &str = "recent";
    let o = args_of(args, TOOL)?;
    let (limit, clamped) = opt_limit(o, TOOL)?;
    let filter = filter_of(o, TOOL, limit)?;
    let rows = queries::list(conn, &filter)?;
    let results: Vec<Value> = rows.iter().map(|e| provenance(e, false)).collect();
    Ok(envelope(TOOL, results, clamped, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_schema_and_they_are_valid_json_schema_objects() {
        for t in TOOLS {
            let s = schema(t.name).unwrap();
            assert_eq!(s["type"], "object", "{}", t.name);
            assert!(s["properties"].is_object(), "{}", t.name);
            assert_eq!(s["additionalProperties"], false, "{}", t.name);
            assert!(!t.description.is_empty());
        }
        assert!(schema("write_memory").is_err(), "there is no write tool");
    }

    /// The three envelopes must agree about the tools, because an agent told
    /// one thing over MCP and another over `tools --schema openai` is an agent
    /// with two different beliefs about the same archive.
    #[test]
    fn the_openai_and_mcp_schemas_describe_the_same_tools() {
        let openai = openai_schema().unwrap();
        let mcp = mcp_schema().unwrap();
        let names_a: Vec<&str> = openai
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        let names_b: Vec<&str> = mcp
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names_a, names_b);
        assert_eq!(names_a, vec!["search_memory", "get_exchange", "recent"]);
        for (a, b) in openai
            .as_array()
            .unwrap()
            .iter()
            .zip(mcp.as_array().unwrap())
        {
            assert_eq!(a["function"]["parameters"], b["inputSchema"]);
            assert_eq!(a["function"]["description"], b["description"]);
        }
    }

    /// A misspelled argument must be an error, not a silently broader search.
    #[test]
    fn an_unknown_argument_is_refused() {
        let e = args_of(&json!({"querry": "ffmpeg"}), "search_memory").unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("unknown argument 'querry'"), "{msg}");
        assert!(msg.contains("query"), "it names what is accepted: {msg}");

        assert!(args_of(&json!({"query": "x", "in": "/tmp"}), "search_memory").is_ok());
        assert!(args_of(&json!([1, 2]), "recent").is_err());
    }

    #[test]
    fn limit_is_clamped_not_rejected_and_says_so() {
        let o = json!({"limit": 5000});
        let (n, clamped) = opt_limit(o.as_object().unwrap(), "recent").unwrap();
        assert_eq!(n, MAX_LIMIT);
        assert!(clamped);
        let o = json!({});
        assert_eq!(
            opt_limit(o.as_object().unwrap(), "recent").unwrap(),
            (DEFAULT_LIMIT, false)
        );
        let o = json!({"limit": 0});
        assert!(opt_limit(o.as_object().unwrap(), "recent").is_err());
        let o = json!({"limit": "ten"});
        assert!(opt_limit(o.as_object().unwrap(), "recent").is_err());
    }

    #[test]
    fn dispatch_refuses_a_tool_that_is_not_one_of_the_three() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for name in ["forget", "import", "capture", "write_memory"] {
            let e = dispatch(&conn, name, &json!({})).unwrap_err();
            assert!(format!("{e:#}").contains("unknown tool"), "{name}");
        }
    }
}
