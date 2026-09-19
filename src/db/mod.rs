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
    let read = std::fs::File::open(path).and_then(|mut f| {
        use std::io::Read;
        f.read_exact(&mut head)
    });
    match read {
        Ok(()) if &head == PLAIN_HEADER => {
            "no    (the file is readable with sqlite3 and grep — `tmem export` if you want it \
             elsewhere)"
                .to_string()
        }
        // Not a plain header. Nothing here can say *what* it is, so it does not
        // guess — and "no" would be a claim contradicted by the bytes.
        Ok(()) => "unknown   (this file does not have a plain SQLite header)".to_string(),
        // The original read this as "plaintext", which printed a statement
        // about the contents of a file it had just failed to read.
        Err(e) => format!("unknown   (could not read the archive header: {e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `status` decides what to print from sixteen bytes of the archive, and
    /// used to treat a *failed read* as "plaintext". An integration test cannot
    /// reach this: `status` opens the database first, so a corrupt file fails
    /// earlier and never gets here.
    #[test]
    fn a_header_it_cannot_read_is_never_reported_as_plaintext() {
        let dir = tempfile::tempdir().unwrap();

        let plain = dir.path().join("plain.db");
        std::fs::write(&plain, b"SQLite format 3\0and then some pages").unwrap();
        assert!(encryption_status(&plain).starts_with("no"));

        let short = dir.path().join("short.db");
        std::fs::write(&short, b"nope").unwrap();
        assert!(
            !encryption_status(&short).starts_with("no"),
            "claimed plaintext from a file it could not read a header from"
        );

        let missing = dir.path().join("missing.db");
        assert!(!encryption_status(&missing).starts_with("no"));

        let enc = dir.path().join("enc.db");
        std::fs::write(&enc, b"\x4a\xff\x00encrypted-bytes-here-x").unwrap();
        assert!(!encryption_status(&enc).starts_with("no"));
    }
}
