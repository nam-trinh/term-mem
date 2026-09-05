//! SQLite access. One file, WAL mode, forward-only migrations from day one —
//! every later phase changes this schema and an unversioned schema becomes an
//! unupgradable one.

pub mod queries;

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

mod embedded {
    refinery::embed_migrations!("src/db/migrations");
}

/// Open (creating if needed) and bring the schema up to date.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }
    let mut conn = Connection::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;
    configure(&conn)?;
    embedded::migrations::runner()
        .run(&mut conn)
        .context("applying database migrations")?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(())
}
