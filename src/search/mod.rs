//! Keyword recall: BM25 over FTS5, with the metadata filters running as SQL
//! predicates alongside the match rather than over its results.
//!
//! No embeddings, no fusion, nothing to configure. docs/plan.md is explicit
//! about why: "If scenario 1 needs embeddings to work, the tokenizer is wrong
//! and adding vectors would hide that."

use crate::db::queries::{self, Exchange, Filter};
use anyhow::{bail, Result};
use rusqlite::{Connection, OptionalExtension};

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
///
/// They are control characters and must never reach a consumer — not a file,
/// not a pipe, and not `--json`, which scenario 3 feeds straight to another
/// assistant.
pub const HL_OPEN: char = '\u{1}';
pub const HL_CLOSE: char = '\u{2}';

/// Strip the sentinels for serialisation. They are an internal detail of how
/// the terminal renderer finds the matched region; a JSON consumer asked for
/// text.
fn plain<S: serde::Serializer>(s: &str, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&strip_markers(s))
}

pub fn strip_markers(s: &str) -> String {
    s.replace([HL_OPEN, HL_CLOSE], "")
}

/// The terms FTS5 actually matched, lifted back out of its own markers. Cheaper
/// and more honest than re-deriving them from the query: stemming means the
/// text that matched is often not the text that was typed.
pub fn matched_terms(snippet: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = snippet;
    while let Some(o) = rest.find(HL_OPEN) {
        let after = &rest[o + HL_OPEN.len_utf8()..];
        let Some(c) = after.find(HL_CLOSE) else { break };
        let term = after[..c].to_lowercase();
        if !term.is_empty() && !out.contains(&term) {
            out.push(term);
        }
        rest = &after[c + HL_CLOSE.len_utf8()..];
    }
    out
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub exchange: Exchange,
    /// The matched region, with the match delimited by [`HL_OPEN`]/[`HL_CLOSE`].
    /// Serialised without them.
    #[serde(serialize_with = "plain")]
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

/// Query terms that match nothing anywhere in the archive.
///
/// Terms are OR-ed, so a term matching nothing does not narrow the result — it
/// simply vanishes, and the results look like an answer to the whole query.
/// `tmem phase5 block` returns exactly what `tmem block` returns, because
/// "Phase 5" is tokenized as two tokens and `phase5` is a third that occurs
/// nowhere. The user has no way to tell, which makes it the failure mode this
/// project keeps naming: it does not look like a failure.
///
/// The probe is a `LIMIT 1` existence check per term, which asks FTS5 for the
/// first docid in a posting list rather than ranking it. It is deliberately run
/// through `MATCH` rather than against `fts5vocab` so the term goes through the
/// same porter stemming as the query it came from.
/// Above this many terms the probe is skipped entirely.
///
/// Each probe measured ~3.4 ms against a 100k-exchange archive, which is fine
/// for the two or three words a person types and is not fine multiplied by the
/// twenty-four a machine can extract from a prompt. A long term list is also
/// where the advice is worthless — "nine of your twenty-four words are not in
/// the archive" is noise, not help.
const MAX_PROBED_TERMS: usize = 8;

pub fn dead_terms(conn: &Connection, terms: &[String]) -> Result<Vec<String>> {
    if terms.len() > MAX_PROBED_TERMS {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT 1 FROM exchanges_fts WHERE exchanges_fts MATCH ?1 LIMIT 1")?;
    let mut dead = Vec::new();
    for t in terms {
        if !t.chars().any(char::is_alphanumeric) {
            continue; // punctuation-only terms are dropped before the query
        }
        let expr = format!("\"{}\"", t.replace('"', "\"\""));
        let found = stmt
            .query_row([&expr], |r| r.get::<_, i64>(0))
            .optional()?
            .is_some();
        if !found {
            dead.push(t.clone());
        }
    }
    Ok(dead)
}

/// `phase5` → `phase 5`, so the hint about a dead term can be concrete.
///
/// Only splits at a letter/digit boundary, which is the shape that actually
/// bites: names like `Phase 5`, `V3`, `utf8` are written both ways by the same
/// person, and the tokenizer keeps them as one token while the archive holds
/// two. Returns `None` when there is nothing to suggest.
pub fn split_alnum_boundary(term: &str) -> Option<String> {
    let mut out = String::with_capacity(term.len() + 1);
    let mut chars = term.chars().peekable();
    let mut split = false;
    while let Some(c) = chars.next() {
        out.push(c);
        if let Some(&n) = chars.peek() {
            if c.is_ascii_alphabetic() && n.is_ascii_digit()
                || c.is_ascii_digit() && n.is_ascii_alphabetic()
            {
                out.push(' ');
                split = true;
            }
        }
    }
    split.then_some(out)
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
        "SELECT e.id, e.assistant, e.session_id, e.thread_id, e.source_key, e.ts, e.cwd, e.repo, \
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
    fn matched_terms_come_back_out_of_the_markers() {
        let s = format!("a {HL_OPEN}Concat{HL_CLOSE} b {HL_OPEN}ffmpeg{HL_CLOSE}");
        assert_eq!(matched_terms(&s), vec!["concat", "ffmpeg"]);
    }

    /// The hint for a dead term has to be concrete to be useful, and `phase5`
    /// is the shape that bites: one token to the tokenizer, two words in the
    /// archive.
    #[test]
    fn a_run_together_name_suggests_where_the_space_goes() {
        assert_eq!(split_alnum_boundary("phase5").as_deref(), Some("phase 5"));
        assert_eq!(split_alnum_boundary("v3").as_deref(), Some("v 3"));
        assert_eq!(
            split_alnum_boundary("utf8mb4").as_deref(),
            Some("utf 8 mb 4")
        );
        // Nothing to suggest for an ordinary word or an already-split one.
        assert_eq!(split_alnum_boundary("deletion"), None);
        assert_eq!(split_alnum_boundary(""), None);
        assert_eq!(split_alnum_boundary("redaction"), None);
    }

    /// The probe costs a query per term, so a machine-generated term list must
    /// not stack them. Measured at ~3.4 ms each against 100k exchanges.
    #[test]
    fn a_long_term_list_is_not_probed() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // No FTS table exists, so any probe would error — returning empty
        // proves it did not run.
        let many: Vec<String> = (0..20).map(|i| format!("word{i}")).collect();
        assert!(dead_terms(&conn, &many).unwrap().is_empty());
    }

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
