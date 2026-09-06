//! A throwaway home directory per test, so nothing here can touch the author's
//! real archive or Claude Code settings.
//!
//! Shared by several integration test binaries; each uses a subset.
#![allow(dead_code)]

use assert_cmd::Command;
use std::path::{Path, PathBuf};

pub struct Env {
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/proj")).unwrap();
        Env { dir }
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    pub fn db(&self) -> PathBuf {
        self.home().join("data/memory.db")
    }

    pub fn projects(&self) -> PathBuf {
        self.home().join("projects")
    }

    pub fn settings(&self) -> PathBuf {
        self.home().join("settings.json")
    }

    /// Copy a checked-in fixture into the fake transcript tree.
    pub fn install(&self, fixture: &str) -> PathBuf {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude_code")
            .join(fixture);
        let dst = self.projects().join("proj").join(fixture);
        std::fs::copy(&src, &dst).unwrap();
        dst
    }

    /// Write a transcript into the fake tree. Used where a test needs paths
    /// that exist on disk (`--repo` resolves a real `.git` at capture time).
    pub fn write_transcript(&self, name: &str, body: &str) -> PathBuf {
        let dst = self.projects().join("proj").join(name);
        std::fs::write(&dst, body).unwrap();
        dst
    }

    pub fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("tmem").unwrap();
        c.env("TMEM_HOME", self.home().join("data"))
            .env("TMEM_CLAUDE_PROJECTS", self.projects())
            .env("TMEM_CLAUDE_SETTINGS", self.settings())
            .env_remove("TMEM");
        c
    }

    /// Ingest one fixture and return its path.
    pub fn ingest(&self, fixture: &str) -> PathBuf {
        let p = self.install(fixture);
        self.cmd()
            .args(["capture", "--path"])
            .arg(&p)
            .assert()
            .success();
        p
    }

    pub fn rows(&self) -> Vec<String> {
        self.query("SELECT id FROM exchanges ORDER BY id")
    }

    pub fn query(&self, sql: &str) -> Vec<String> {
        let conn = rusqlite::Connection::open(self.db()).unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        let out = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        out
    }

    pub fn count(&self, table: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.db()).unwrap();
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}
