//! Keyword recall: BM25 over FTS5, with the metadata filters running as SQL
//! predicates alongside the match rather than over its results.
//!
//! No embeddings, no fusion, nothing to configure. docs/plan.md is explicit
//! about why: "If scenario 1 needs embeddings to work, the tokenizer is wrong
//! and adding vectors would hide that."

use crate::db::queries::{self, Exchange, Filter};
use anyhow::{bail, Result};
use rusqlite::Connection;

/// Column weights for `bm25()`, in the column order of `exchanges_fts`.
///
/// docs/tech-stack.md asks for `commands` ≫ `prompt` > `response`. The ratios
/// are a judgement, not a measurement — the ranking function is the part of
/// this project that is explicitly allowed to be replaced later, and the
/// scenarios are what say whether it is good enough.
const W_PROMPT: f64 = 2.0;
const W_RESPONSE: f64 = 1.0;
const W_COMMANDS: f64 = 8.0;

/// Sentinels wrapped around the matched region by FTS5, swapped for terminal
/// escapes (or removed) once we know whether stdout is a terminal.
pub const HL_OPEN: &str = "\u{1}";
pub const HL_CLOSE: &str = "\u{2}";

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub exchange: Exchange,
    /// The matched region, with the match delimited by [`HL_OPEN`]/[`HL_CLOSE`].
    pub snippet: String,
    /// BM25, negated so that larger is better — the raw value is negative and
    /// sorts the other way, which is a trap in a `--json` consumer.
    pub score: f64,
}

/// Turn bare `argv` into an FTS5 MATCH expression.
///
/// Every term becomes a quoted string, which is what makes this safe: inside
/// double quotes FTS5 treats `-`, `*`, `:`, `(`, `NOT` and the rest as ordinary
/// text, so a query is never able to become syntax. Terms are OR-ed, which is
/// what lets `tmem <query>` accept bare multi-word input with no quoting —
/// docs/cli.md — and BM25 is left to sort out which of them mattered.
pub fn build_match(terms: &[String]) -> Result<String> {
    let mut parts = Vec::new();
    for t in terms {
        // A term of pure punctuation contributes no tokens and would make the
        // expression `"" OR x`, which FTS5 rejects outright.
        if !t.chars().any(char::is_alphanumeric) {
            continue;
        }
        parts.push(format!("\"{}\"", t.replace('"', "\"\"")));
    }
    if parts.is_empty() {
        bail!("nothing to search for — the query has no searchable terms");
    }
    Ok(parts.join(" OR "))
}

pub fn search(conn: &Connection, terms: &[String], filter: &Filter) -> Result<Vec<Hit>> {
    let expr = build_match(terms)?;
    let (where_sql, filter_args) = filter.clauses();

    // `?1` for the MATCH, then the filter's bare `?` placeholders, which SQLite
    // numbers from 2 because they appear later in the statement.
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(expr)];
    args.extend(filter_args);

    // The metadata filters sit in the same WHERE as the MATCH. docs/tech-stack.md
    // calls them a pre-filter, and semantically they are — they constrain the
    // candidate set rather than trimming a ranked list, so a `--repo` search
    // returns the best two matches in that repo, not the ones that survived the
    // global top twenty.
    let sql = format!(
        "SELECT e.id, e.assistant, e.session_id, e.thread_id, e.ts, e.cwd, e.repo, \
                e.git_branch, e.model, e.prompt, e.response, e.redacted, \
                -bm25(exchanges_fts, {W_PROMPT}, {W_RESPONSE}, {W_COMMANDS}) AS score, \
                snippet(exchanges_fts, -1, '{HL_OPEN}', '{HL_CLOSE}', '…', 24) AS snippet \
         FROM exchanges_fts JOIN exchanges e ON e.rowid = exchanges_fts.rowid \
         WHERE exchanges_fts MATCH ?1{} \
         ORDER BY score DESC, e.ts DESC LIMIT {}",
        where_sql,
        filter.limit.unwrap_or(usize::MAX >> 1)
    );

    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let mut hits: Vec<Hit> = stmt
        .query_map(refs.as_slice(), |row| {
            Ok(Hit {
                exchange: queries::row_to_exchange(row)?,
                score: row.get("score")?,
                snippet: row.get("snippet")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut rows: Vec<Exchange> = hits.iter().map(|h| h.exchange.clone()).collect();
    queries::hydrate(conn, &mut rows)?;
    for (h, r) in hits.iter_mut().zip(rows) {
        h.exchange = r;
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_or_ed_and_quoted() {
        assert_eq!(
            build_match(&["ffmpeg".into(), "concat".into()]).unwrap(),
            "\"ffmpeg\" OR \"concat\""
        );
    }

    /// FTS5 syntax inside a query term is text, never syntax. A user typing
    /// `tmem search NOT` or a path with a `*` in it must not get an error, and
    /// must not get a different query than they asked for.
    #[test]
    fn fts_syntax_in_a_term_is_inert() {
        assert_eq!(build_match(&["NOT".into()]).unwrap(), "\"NOT\"");
        assert_eq!(build_match(&["a*b".into()]).unwrap(), "\"a*b\"");
        assert_eq!(build_match(&["col:val".into()]).unwrap(), "\"col:val\"");
        assert_eq!(
            build_match(&["say \"hi\"".into()]).unwrap(),
            "\"say \"\"hi\"\"\""
        );
    }

    #[test]
    fn punctuation_only_terms_are_dropped_not_passed_through() {
        assert_eq!(
            build_match(&["--".into(), "ffmpeg".into()]).unwrap(),
            "\"ffmpeg\""
        );
        assert!(build_match(&["--".into()]).is_err());
        assert!(build_match(&[]).is_err());
    }
}
