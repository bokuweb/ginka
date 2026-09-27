//! Where this app keeps what little it keeps.
//!
//! `~/.e1/` holds the window's settings and its logs, and nothing else: the
//! token is discovered, not stored (`AGENTS.md` rule 8), and GitHub's data is
//! cached in memory only. `E1_HOME` moves the directory, which is how a test
//! or a second profile keeps out of the real one.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The root and the files under it.
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// `E1_HOME`, or `~/.e1`.
    pub fn from_env() -> Result<Self> {
        if let Some(root) = std::env::var_os("E1_HOME") {
            return Ok(Self::with_root(root));
        }
        let home = dirs::home_dir().context("no home directory")?;
        Ok(Self::with_root(home.join(".e1")))
    }

    /// Rooted somewhere specific.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The window's settings: `app.json`.
    pub fn app_settings(&self) -> PathBuf {
        self.root.join("app.json")
    }

    /// Daily-rotated logs.
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// What GitHub said, kept: safe to delete at any time.
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Answers with their `ETag`s, one file per URL.
    pub fn http_cache(&self) -> PathBuf {
        self.cache().join("http")
    }

    /// The store's memory for the next launch.
    pub fn snapshot(&self) -> PathBuf {
        self.cache().join("store.json")
    }

    /// Create the directories, which is safe to repeat.
    pub fn ensure(&self) -> Result<()> {
        for dir in [self.logs(), self.http_cache()] {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(())
    }
}
