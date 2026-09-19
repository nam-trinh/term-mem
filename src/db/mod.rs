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

/// What to print for `status`'s `encrypted` line.
///
/// Read from the file rather than from a setting, so it cannot claim a property
/// the bytes on disk do not have. A plain SQLite database begins with the
/// sixteen bytes `SQLite format 3\0`; SQLCipher and friends encrypt from byte
/// zero, so their absence is the signal.
///
/// Phase 3 did **not** ship encryption — see docs/phases/phase-3.md finding 5
/// for why the key management, not the cipher, is what blocks it.
pub fn encryption_status(path: &Path) -> String {
    const PLAIN_HEADER: &[u8; 16] = b"SQLite format 3\0";
    let mut head = [0u8; 16];
    let looks_plain = std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut head)
        })
        .map(|()| &head == PLAIN_HEADER)
        .unwrap_or(true);
    if looks_plain {
        "no    (the file is readable with sqlite3 and grep — `tmem export` if you want it elsewhere)"
            .to_string()
    } else {
        "the file does not have a plain SQLite header".to_string()
    }
}
