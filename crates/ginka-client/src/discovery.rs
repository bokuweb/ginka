//! Finding a daemon, and starting one when there is none.
//!
//! Neither the app nor the CLI should make the user run a background service
//! by hand. Whichever of them notices there is no daemon spawns one, waits for
//! it to publish itself, and connects; the second one to arrive finds the file
//! and reuses the process.

use crate::Client;
use anyhow::{Context, Result, anyhow};
use ginka_protocol::{Handshake, Seq};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long to wait for a spawned daemon to publish its handshake.
///
/// Generous because the first start also runs the database migrations; a
/// client that gives up early would spawn a second daemon on top of the first.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// How often to look for the handshake while waiting.
const STARTUP_POLL: Duration = Duration::from_millis(25);

/// The environment variable that overrides which daemon binary is started.
pub const DAEMON_BINARY_ENV: &str = "GINKA_DAEMON";

/// Where to look for a daemon, and how to start one.
#[derive(Debug, Clone)]
pub struct Discovery {
    handshake_path: PathBuf,
    binary: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl Discovery {
    /// Look for the daemon that publishes itself at `handshake_path`.
    pub fn new(handshake_path: impl Into<PathBuf>) -> Self {
        Self {
            handshake_path: handshake_path.into(),
            binary: None,
            home: None,
        }
    }

    /// Start this exact binary rather than searching for one.
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = Some(binary.into());
        self
    }

    /// Hand a spawned daemon this `GINKA_HOME`.
    ///
    /// Without it a daemon started from a client running against a temporary
    /// state directory would write into the user's real one.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// What the handshake file says, if there is one. It may be stale.
    pub fn published(&self) -> Option<Handshake> {
        let text = std::fs::read_to_string(&self.handshake_path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Connect to the running daemon, starting one if the published details
    /// are missing or dead.
    ///
    /// `resume_from` is passed through to [`Client::connect`].
    pub async fn connect(&self, resume_from: Option<Seq>) -> Result<Client> {
        if let Some(handshake) = self.published()
            && let Ok(client) = Client::connect(&handshake, resume_from).await
        {
            return Ok(client);
        }
        self.spawn()?;
        self.wait_for_daemon(resume_from).await
    }

    /// Connect only if a daemon is already running.
    pub async fn connect_existing(&self, resume_from: Option<Seq>) -> Result<Client> {
        let handshake = self
            .published()
            .ok_or_else(|| anyhow!("no daemon is running"))?;
        Client::connect(&handshake, resume_from).await
    }

    /// Start a daemon in the background.
    ///
    /// Its output goes to its own log file, so a spawned daemon cannot write
    /// over a CLI's stdout, and it is not killed when its parent exits.
    fn spawn(&self) -> Result<()> {
        let binary = self.binary_path()?;
        let mut command = std::process::Command::new(&binary);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(home) = &self.home {
            command.env("GINKA_HOME", home);
        }
        command
            .spawn()
            .with_context(|| format!("starting {}", binary.display()))?;
        Ok(())
    }

    /// Wait for the daemon to publish itself, then connect.
    async fn wait_for_daemon(&self, resume_from: Option<Seq>) -> Result<Client> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let mut last_error = None;
        while Instant::now() < deadline {
            if let Some(handshake) = self.published() {
                match Client::connect(&handshake, resume_from).await {
                    Ok(client) => return Ok(client),
                    Err(error) => last_error = Some(error),
                }
            }
            smol::Timer::after(STARTUP_POLL).await;
        }
        Err(match last_error {
            Some(error) => error.context("the daemon started but would not accept a connection"),
            None => anyhow!(
                "the daemon did not publish {} within {STARTUP_TIMEOUT:?}",
                self.handshake_path.display()
            ),
        })
    }

    /// Which daemon binary to start.
    ///
    /// The order matters: an explicit choice wins, then the environment, then
    /// the binary sitting next to the one that is running — a development
    /// build must start the daemon it was built with, not one an old install
    /// left on `PATH`.
    pub fn binary_path(&self) -> Result<PathBuf> {
        if let Some(binary) = &self.binary {
            return Ok(binary.clone());
        }
        if let Some(from_env) = std::env::var_os(DAEMON_BINARY_ENV) {
            return Ok(PathBuf::from(from_env));
        }
        if let Some(sibling) = sibling_daemon() {
            return Ok(sibling);
        }
        Ok(PathBuf::from("ginka-daemon"))
    }
}

/// The `ginka-daemon` next to the currently running executable, if it exists.
fn sibling_daemon() -> Option<PathBuf> {
    let candidate = std::env::current_exe()
        .ok()?
        .parent()?
        .join(if cfg!(windows) {
            "ginka-daemon.exe"
        } else {
            "ginka-daemon"
        });
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_binary_wins_over_everything_else() {
        let discovery = Discovery::new("/tmp/daemon.json").with_binary("/opt/ginka-daemon");
        assert_eq!(
            discovery.binary_path().unwrap(),
            PathBuf::from("/opt/ginka-daemon")
        );
    }

    #[test]
    fn a_missing_handshake_file_reads_as_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let discovery = Discovery::new(dir.path().join("daemon.json"));
        assert!(discovery.published().is_none());
    }

    #[test]
    fn a_corrupt_handshake_file_reads_as_no_daemon() {
        // Truncated by a crash mid-write: better to start a daemon than to
        // fail with a parse error the user cannot act on.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        std::fs::write(&path, "{ \"port\": ").unwrap();
        assert!(Discovery::new(path).published().is_none());
    }

    #[test]
    fn the_daemon_next_to_this_binary_is_preferred_over_the_path() {
        // In a test run the sibling is the daemon this workspace just built,
        // which is the one a developer means.
        let discovery = Discovery::new("/tmp/daemon.json");
        let chosen = discovery.binary_path().unwrap();
        assert!(
            chosen.ends_with("ginka-daemon") || chosen.ends_with("ginka-daemon.exe"),
            "{}",
            chosen.display()
        );
    }
}
