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

/// Open for reading only, and fail rather than create.
///
/// This is what backs the "agents read memory; they never write or delete it"
/// rule in docs/plan.md. A read-only *flag* on a normal connection would be a
/// promise; `SQLITE_OPEN_READ_ONLY` is a property of the handle, so a tool that
/// tried to write would get `attempt to write a readonly database` from SQLite
/// itself rather than from our good intentions.
///
/// Migrations are deliberately not run here — they cannot be, on a read-only
/// handle. An archive older than the binary fails loudly on the first query,
/// which is the right way round: `tmem status` is one write-capable command
/// away and will migrate it.
pub fn open_readonly(path: &Path) -> Result<Connection> {
    if !path.exists() {
        anyhow::bail!("no archive at {} — run `tmem init` first", path.display());
    }
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening database at {} read-only", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
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

    /// docs/plan.md, Phase 4: "Agents read memory; they never write or delete
    /// it." That is enforced by the open flags, so this is the test that the
    /// flags are the ones claimed — every agent-facing path in the binary goes
    /// through `open_readonly` and inherits whatever this handle allows.
    #[test]
    fn a_read_only_handle_cannot_write_however_it_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        {
            let conn = open(&path).unwrap();
            conn.execute_batch(
                "INSERT INTO exchanges (id, assistant, session_id, thread_id, source_key, ts, \
                 cwd, prompt, response) VALUES \
                 ('01A','claude-code','s','t','k',0,'/home/dev','q','a')",
            )
            .unwrap();
        }
        let ro = open_readonly(&path).unwrap();
        assert_eq!(
            ro.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1,
            "reading still works"
        );
        for sql in [
            "DELETE FROM exchanges",
            "UPDATE exchanges SET prompt = 'x'",
            "INSERT INTO exchanges (id) VALUES ('02B')",
            "DROP TABLE exchanges",
            "CREATE TABLE evil (x)",
            "INSERT INTO exchanges_fts(exchanges_fts) VALUES('rebuild')",
        ] {
            assert!(
                ro.execute_batch(sql).is_err(),
                "a read-only handle ran: {sql}"
            );
        }
        // And the archive is untouched.
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    /// `open` creates; `open_readonly` must not — an agent pointed at the wrong
    /// path should be told, not handed an empty archive that looks like a user
    /// with no history.
    #[test]
    fn open_readonly_refuses_to_create_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nothing-here.db");
        let e = open_readonly(&path).unwrap_err();
        assert!(format!("{e:#}").contains("tmem init"), "{e:#}");
        assert!(!path.exists(), "it created the file it was asked to read");
    }

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
