//! Where Ginka keeps its state on disk.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Every on-disk location Ginka owns, resolved once.
///
/// The root is overridable through `GINKA_HOME`, so tests run against a
/// temporary directory and a development build can be kept away from a
/// release install's state (`GINKA_HOME=~/.ginka-dev scripts/dev-macos`).
/// A debug build does not move it on its own: doing so would make every
/// existing development setup look empty. See `docs/roadmap.md` §4.1.
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Resolve from the environment: `GINKA_HOME` if set, otherwise `~/.ginka`.
    pub fn from_env() -> Result<Self> {
        let root = match std::env::var_os("GINKA_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => dirs::home_dir()
                .context("no home directory; set GINKA_HOME to choose a state directory")?
                .join(".ginka"),
        };
        Ok(Self { root })
    }

    /// Use `root` as the state directory as is, ignoring `GINKA_HOME`; for tests
    /// and callers that already resolved it. Nothing is created until [`Paths::ensure`].
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The state directory every other path hangs off.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// UI-owned settings: window state, theme choice, last workspace.
    pub fn app_settings(&self) -> PathBuf {
        self.root.join("app.json")
    }

    /// Daemon-owned settings: agents, poll intervals, retention.
    pub fn daemon_settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// The SQLite database the daemon owns, `ginka.db`.
    pub fn database(&self) -> PathBuf {
        self.root.join("ginka.db")
    }

    /// Port + token the app and CLI use to find a running daemon.
    pub fn daemon_handshake(&self) -> PathBuf {
        self.root.join("daemon.json")
    }

    /// The lock only one daemon at a time can hold for this state directory.
    pub fn daemon_lock(&self) -> PathBuf {
        self.root.join(ginka_protocol::handshake::DAEMON_LOCK_FILE)
    }

    /// The cached public rate table (§3.3 N13), beside the database.
    pub fn rates_cache(&self) -> PathBuf {
        self.root.join("rates.json")
    }

    /// Daily-rotated log files, one set per process (`logging::init`).
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Worktrees Ginka creates live under the project's own directory here, so
    /// they never pollute the user's repository checkout.
    pub fn worktrees(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    /// Scratch workspaces for "just start an agent, no project" flows.
    pub fn scratch_projects(&self) -> PathBuf {
        self.root.join("projects")
    }

    /// Files the user attached to a message. Daemon-owned: a client sends
    /// bytes and gets a reference back, never a path (§4.1).
    pub fn attachments(&self) -> PathBuf {
        self.root.join("attachments")
    }

    /// Binary payloads an agent emitted, content-addressed (§3.3 N6).
    pub fn blobs(&self) -> PathBuf {
        self.root.join("blobs")
    }

    /// One directory per account, each the home a vendor's CLI keeps one
    /// login in (`docs/accounts.md` §3). The directories themselves are made
    /// private when an account is added, not here.
    pub fn accounts(&self) -> PathBuf {
        self.root.join("accounts")
    }

    /// The embedded browser's profile: its cache, and the cookies it keeps
    /// or was given from Chrome. Private to the user, like the connectors'
    /// secrets.
    pub fn browser(&self) -> PathBuf {
        self.root.join("browser")
    }

    /// Chat connectors' secrets, one `<connector>.env` per connector, each
    /// `0600` (`docs/connectors.md` §4.1). Tokens live here or in the
    /// environment, and nowhere else.
    pub fn connectors(&self) -> PathBuf {
        self.root.join("connectors")
    }

    /// The Slack connector's tokens.
    pub fn slack_secrets(&self) -> PathBuf {
        self.connectors().join("slack.env")
    }

    /// Create every directory Ginka writes into. Idempotent.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.logs(),
            self.worktrees(),
            self.scratch_projects(),
            self.attachments(),
            self.blobs(),
            self.accounts(),
            self.connectors(),
        ] {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let browser = self.browser();
        std::fs::create_dir_all(&browser)
            .with_context(|| format!("creating {}", browser.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("making {} private", browser.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_creates_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(tmp.path().join("state"));
        paths.ensure().unwrap();
        assert!(paths.logs().is_dir());
        assert!(paths.worktrees().is_dir());
        // Idempotent: a second call on an existing tree must not fail.
        paths.ensure().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_browser_profile_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(tmp.path().join("state"));
        paths.ensure().unwrap();
        let mode = std::fs::metadata(paths.browser())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "cookies live here");
    }

    #[test]
    fn files_hang_off_the_root() {
        let paths = Paths::with_root("/tmp/ginka-test");
        assert!(paths.database().starts_with("/tmp/ginka-test"));
        assert_ne!(paths.app_settings(), paths.daemon_settings());
    }
}
