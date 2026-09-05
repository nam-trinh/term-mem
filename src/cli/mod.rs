pub mod capture_cmd;
pub mod forget;
pub mod ignore;
pub mod init;
pub mod pause;
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

impl BrowseArgs {
    pub fn to_filter(&self) -> Result<Filter> {
        let in_path = self.in_path.as_ref().map(|p| {
            std::fs::canonicalize(p)
                .unwrap_or_else(|_| p.clone())
                .to_string_lossy()
                .into_owned()
        });
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
            in_path,
            since_ms: self.since.as_deref().map(timespec::parse).transpose()?,
            repo,
            limit: Some(self.limit),
        })
    }
}
