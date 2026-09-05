//! Everything that reads or writes rows. Kept in one place so `forget` can be
//! audited against it: a delete that misses a derived table is a delete that
//! leaves the text on disk.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Exchange {
    pub id: String,
    pub assistant: String,
    pub session_id: String,
    pub thread_id: String,
    pub ts: i64,
    pub cwd: String,
    pub repo: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub response: String,
    pub redacted: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

fn row_to_exchange(row: &Row) -> rusqlite::Result<Exchange> {
    Ok(Exchange {
        id: row.get("id")?,
        assistant: row.get("assistant")?,
        session_id: row.get("session_id")?,
        thread_id: row.get("thread_id")?,
        ts: row.get("ts")?,
        cwd: row.get("cwd")?,
        repo: row.get("repo")?,
        git_branch: row.get("git_branch")?,
        model: row.get("model")?,
        prompt: row.get("prompt")?,
        response: row.get("response")?,
        redacted: row.get::<_, i64>("redacted")? != 0,
        commands: Vec::new(),
        files: Vec::new(),
    })
}

const SELECT: &str = "SELECT id, assistant, session_id, thread_id, ts, cwd, repo, \
                      git_branch, model, prompt, response, redacted FROM exchanges";

/// Attach mined commands and file references. Done as a second pass rather than
/// a join so a single exchange with 40 commands doesn't fan the result set out.
pub fn hydrate(conn: &Connection, rows: &mut [Exchange]) -> Result<()> {
    let mut cmds = conn.prepare("SELECT cmd FROM commands WHERE exchange_id = ?1 ORDER BY seq")?;
    let mut files =
        conn.prepare("SELECT path FROM file_refs WHERE exchange_id = ?1 ORDER BY seq")?;
    for ex in rows.iter_mut() {
        ex.commands = cmds
            .query_map(params![ex.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        ex.files = files
            .query_map(params![ex.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(())
}

/// Browse filters. Phase 1 has no text query; these are the whole surface.
#[derive(Default, Debug, Clone)]
pub struct Filter {
    pub in_path: Option<String>,
    pub since_ms: Option<i64>,
    pub repo: Option<String>,
    pub limit: Option<usize>,
}

impl Filter {
    fn clauses(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut sql = String::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(p) = &self.in_path {
            // Prefix match on the directory tree, with the separator appended so
            // `--in ~/src/api` does not also match `~/src/api-legacy`.
            sql.push_str(" AND (cwd = ? OR cwd GLOB ?)");
            args.push(Box::new(p.clone()));
            args.push(Box::new(format!("{}/*", p.trim_end_matches('/'))));
        }
        if let Some(t) = self.since_ms {
            sql.push_str(" AND ts >= ?");
            args.push(Box::new(t));
        }
        if let Some(r) = &self.repo {
            sql.push_str(" AND repo = ?");
            args.push(Box::new(r.clone()));
        }
        (sql, args)
    }
}

pub fn list(conn: &Connection, filter: &Filter) -> Result<Vec<Exchange>> {
    let (where_sql, args) = filter.clauses();
    let sql = format!(
        "{SELECT} WHERE 1=1{where_sql} ORDER BY ts DESC, id DESC LIMIT {}",
        filter.limit.unwrap_or(usize::MAX >> 1)
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let mut rows: Vec<Exchange> = stmt
        .query_map(refs.as_slice(), row_to_exchange)?
        .collect::<rusqlite::Result<_>>()?;
    hydrate(conn, &mut rows)?;
    Ok(rows)
}

/// Resolve an id, accepting a unique prefix — ULIDs are 26 characters and
/// nobody is going to retype one.
pub fn resolve_id(conn: &Connection, id: &str) -> Result<Option<String>> {
    let exact: Option<String> = conn
        .query_row("SELECT id FROM exchanges WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?;
    if exact.is_some() {
        return Ok(exact);
    }
    let mut stmt = conn.prepare("SELECT id FROM exchanges WHERE id LIKE ?1 || '%' LIMIT 2")?;
    let hits: Vec<String> = stmt
        .query_map(params![id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    match hits.len() {
        1 => Ok(Some(hits[0].clone())),
        0 => Ok(None),
        _ => anyhow::bail!("id prefix '{id}' is ambiguous; use more characters"),
    }
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Exchange>> {
    let mut rows: Vec<Exchange> = conn
        .prepare(&format!("{SELECT} WHERE id = ?1"))?
        .query_map(params![id], row_to_exchange)?
        .collect::<rusqlite::Result<_>>()?;
    hydrate(conn, &mut rows)?;
    Ok(rows.pop())
}

/// The whole conversation tree around an exchange — grouped on `thread_id`, not
/// `session_id`. Phase 0 finding 5: `/clear` starts a fresh tree inside the same
/// transcript file under the same session id.
pub fn thread(conn: &Connection, id: &str) -> Result<Vec<Exchange>> {
    let sql = format!(
        "{SELECT} WHERE (assistant, session_id, thread_id) = \
         (SELECT assistant, session_id, thread_id FROM exchanges WHERE id = ?1) \
         ORDER BY ts ASC, id ASC"
    );
    let mut rows: Vec<Exchange> = conn
        .prepare(&sql)?
        .query_map(params![id], row_to_exchange)?
        .collect::<rusqlite::Result<_>>()?;
    hydrate(conn, &mut rows)?;
    Ok(rows)
}

pub fn last_id(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM exchanges ORDER BY ts DESC, id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// A genuine delete: the row and every derived artifact, in one transaction,
/// followed by VACUUM so the text is not recoverable from a free page.
/// Callers must not add a `deleted` flag here. See docs/mission.md.
pub fn forget(conn: &mut Connection, ids: &[String]) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut n = 0;
    for id in ids {
        tx.execute("DELETE FROM commands  WHERE exchange_id = ?1", params![id])?;
        tx.execute("DELETE FROM file_refs WHERE exchange_id = ?1", params![id])?;
        n += tx.execute("DELETE FROM exchanges WHERE id = ?1", params![id])?;
    }
    tx.commit()?;
    // VACUUM cannot run inside a transaction, and rewrites the file without the
    // freed pages. WAL checkpoint first so the deleted text is not left behind
    // in -wal.
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    conn.execute_batch("VACUUM")?;
    Ok(n)
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get(0))?)
}
