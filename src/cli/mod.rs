pub mod capture_cmd;
pub mod forget;
pub mod ignore;
pub mod init;
pub mod pause;
pub mod search;
pub mod show;
pub mod status;
pub mod timespec;

use crate::db::queries::Filter;
use anyhow::Result;
use std::path::PathBuf;

/// Shared browse filters. `--in` defaults to global, never to the current
/// directory — scenarios.md is explicit that an implicit `--in .` breaks
/// scenario 2, and breaks it silently.
#[derive(Debug, clap::Args, Default)]
pub struct BrowseArgs {
    /// Limit to a directory tree
    #[arg(long = "in", value_name = "PATH")]
    pub in_path: Option<PathBuf>,
    /// Limit by time, e.g. `2h`, `7d`, `2026-03-01`
    #[arg(long, value_name = "WHEN")]
    pub since: Option<String>,
    /// Limit to the current git repository
    #[arg(long)]
    pub repo: bool,
    /// Maximum results
    #[arg(long, short = 'n', default_value_t = 20)]
    pub limit: usize,
    /// Machine-readable output, one JSON record per line
    #[arg(long)]
    pub json: bool,
}

/// Both spellings of a `--in` path: as typed, and as the filesystem resolves
/// it. They differ wherever a symlink is involved — `/tmp` and `/var` on macOS
/// are the everyday case — and the archive holds whichever one the assistant
/// happened to record.
pub fn in_path_candidates(p: &std::path::Path) -> Vec<String> {
    let mut out = vec![p.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(p) {
        let real = real.to_string_lossy().into_owned();
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out
}

impl BrowseArgs {
    pub fn to_filter(&self) -> Result<Filter> {
        let in_paths = self
            .in_path
            .as_ref()
            .map(|p| in_path_candidates(p))
            .unwrap_or_default();
        let repo = if self.repo {
            let cwd = std::env::current_dir()?;
            match crate::capture::resolve_repo(&cwd) {
                Some(r) => Some(r),
                None => anyhow::bail!("--repo: {} is not inside a git repository", cwd.display()),
            }
        } else {
            None
        };
        Ok(Filter {
            in_paths,
            since_ms: self.since.as_deref().map(timespec::parse).transpose()?,
            repo,
            limit: Some(self.limit),
        })
    }
}
