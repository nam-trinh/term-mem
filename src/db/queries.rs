//! Everything that reads or writes rows. Kept in one place so `forget` can be
//! audited against it: a delete that misses a derived table is a delete that
//! leaves the text on disk.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Exchange {
    pub id: String,
    pub assistant: String,
    pub session_id: String,
    pub thread_id: String,
    /// The adapter's dedup key. Carried in `--json` so that an export is
    /// re-importable without inventing identity, and so a result can be traced
    /// back to the record it came from.
    pub source_key: String,
    pub ts: i64,
    pub cwd: String,
    pub repo: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub response: String,
    pub redacted: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub commands: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files: Vec<String>,
}

pub fn row_to_exchange(row: &Row) -> rusqlite::Result<Exchange> {
    Ok(Exchange {
        id: row.get("id")?,
        assistant: row.get("assistant")?,
        session_id: row.get("session_id")?,
        thread_id: row.get("thread_id")?,
        source_key: row.get("source_key")?,
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

const SELECT: &str = "SELECT id, assistant, session_id, thread_id, source_key, ts, cwd, repo, \
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
    /// Candidate spellings of one `--in` tree. A path can reach the archive in
    /// more than one form — on macOS `/var/…` and `/private/var/…` name the
    /// same directory — and the stored `cwd` is whatever the assistant wrote,
    /// not whatever the user types later. Matching any of them is the
    /// difference between a filter that works and one that silently returns
    /// nothing, which is the failure docs/scenarios.md warns about.
    pub in_paths: Vec<String>,
    pub since_ms: Option<i64>,
    pub repo: Option<String>,
    pub limit: Option<usize>,
}

impl Filter {
    pub fn clauses(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut sql = String::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if !self.in_paths.is_empty() {
            // Prefix match on the directory tree, with the separator appended so
            // `--in ~/src/api` does not also match `~/src/api-legacy`.
            //
            // Compared with `substr`, not `GLOB` or `LIKE`: a path is user data
            // and may contain `*`, `?` or `[`, which a pattern match would treat
            // as wildcards. That fails *silently* — `--in '/home/a[1]'` returns
            // nothing and looks like the archive lost the exchange, which is the
            // exact failure docs/scenarios.md warns about. `LIKE` is no better;
            // it is also ASCII-case-insensitive, and paths are not.
            let mut ors = Vec::new();
            for p in &self.in_paths {
                let dir = p.trim_end_matches('/');
                let prefix = format!("{dir}/");
                ors.push("cwd = ? OR substr(cwd, 1, ?) = ?");
                args.push(Box::new(dir.to_string()));
                args.push(Box::new(prefix.chars().count() as i64));
                args.push(Box::new(prefix));
            }
            sql.push_str(&format!(" AND ({})", ors.join(" OR ")));
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
///
/// A tombstone of the adapter's dedup key is kept, and only that — otherwise
/// the next ingest of the same transcript puts the exchange straight back.
pub fn forget(conn: &mut Connection, ids: &[String]) -> Result<usize> {
    let now = crate::capture::now_ms();
    let tx = conn.transaction()?;
    let mut n = 0;
    for id in ids {
        let key: Option<(String, String, String)> = tx
            .query_row(
                "SELECT assistant, session_id, source_key FROM exchanges WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((assistant, session, source)) = key {
            tx.execute(
                "INSERT INTO forgotten (assistant, session_id, source_key, forgotten_at) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
                params![assistant, session, source, now],
            )?;
        }
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

pub fn forgotten_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM forgotten", [], |r| r.get(0))?)
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get(0))?)
}

pub enum Imported {
    Inserted,
    AlreadyPresent,
    /// The user deleted this exchange. `forget` is irreversible on purpose, and
    /// an import is not a way around it.
    Forgotten,
}

/// Write one exchange that came from an export rather than from a transcript.
///
/// Keyed on the same `(assistant, session_id, source_key)` as ingest, so
/// importing the same file twice is a no-op and importing a file that overlaps
/// the live archive does not duplicate it.
pub fn import_exchange(tx: &rusqlite::Transaction, ex: &Exchange) -> Result<Imported> {
    let tombstoned: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM forgotten \
         WHERE assistant = ?1 AND session_id = ?2 AND source_key = ?3)",
        params![&ex.assistant, &ex.session_id, &ex.source_key],
        |r| r.get::<_, i64>(0),
    )? != 0;
    if tombstoned {
        return Ok(Imported::Forgotten);
    }
    let present: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM exchanges \
         WHERE assistant = ?1 AND session_id = ?2 AND source_key = ?3)",
        params![&ex.assistant, &ex.session_id, &ex.source_key],
        |r| r.get::<_, i64>(0),
    )? != 0;
    if present {
        return Ok(Imported::AlreadyPresent);
    }

    // The id from the export is kept when it is free, so ids in someone's notes
    // keep resolving across a backup and restore.
    let id_taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM exchanges WHERE id = ?1)",
        params![&ex.id],
        |r| r.get::<_, i64>(0),
    )? != 0;
    let id = if id_taken {
        ulid::Ulid::from_parts(ex.ts.max(0) as u64, crate::capture::rand_u128()).to_string()
    } else {
        ex.id.clone()
    };

    tx.execute(
        "INSERT INTO exchanges (id, assistant, session_id, thread_id, source_key, ts, cwd, \
         repo, git_branch, model, prompt, response, commands_text, redacted) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            &id,
            &ex.assistant,
            &ex.session_id,
            &ex.thread_id,
            &ex.source_key,
            ex.ts,
            &ex.cwd,
            &ex.repo,
            &ex.git_branch,
            &ex.model,
            &ex.prompt,
            &ex.response,
            ex.commands.join("\n"),
            ex.redacted as i64
        ],
    )?;
    for (i, c) in ex.commands.iter().enumerate() {
        tx.execute(
            "INSERT INTO commands (exchange_id, seq, cmd, lang) VALUES (?1,?2,?3,NULL)",
            params![&id, i as i64, c],
        )?;
    }
    for (i, f) in ex.files.iter().enumerate() {
        tx.execute(
            "INSERT INTO file_refs (exchange_id, seq, path, tool) VALUES (?1,?2,?3,'import')",
            params![&id, i as i64, f],
        )?;
    }
    Ok(Imported::Inserted)
}
