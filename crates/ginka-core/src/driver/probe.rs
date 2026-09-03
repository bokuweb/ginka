//! Asking an agent CLI whether it is there, and whether it will work.
//!
//! An agent that is not installed, or installed but not signed in, is
//! something the user has to be told before they send a prompt. Finding out
//! from a session that failed is finding out too late — and it is what the
//! transcript says instead of the answer they wanted.
//!
//! The probe runs the vendor's own commands in the same sanitized environment
//! a session would get, so what it reports is what a session would meet.

use super::{AgentDriver, CommandSpec};
use ginka_protocol::model::AgentStatus;
use std::time::Duration;

/// How long a vendor's CLI is given to answer a probe.
///
/// Long enough for a cold start of a Node binary, short enough that one hung
/// CLI does not hold up the answer for the others.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ask one agent about itself.
pub fn probe(driver: &dyn AgentDriver) -> AgentStatus {
    let version_output = run(&driver.probe_command());
    let installed = version_output.is_some();
    let version = version_output
        .as_deref()
        .and_then(|output| driver.parse_version(output));

    // Only worth asking if the binary answered at all.
    let (authenticated, detail) = match (installed, driver.auth_command()) {
        (true, Some(command)) => {
            match run(&command).and_then(|output| driver.parse_auth(&output)) {
                Some((signed_in, detail)) => (Some(signed_in), detail),
                None => (None, None),
            }
        }
        _ => (None, None),
    };

    AgentStatus {
        id: driver.id().to_string(),
        display_name: driver.display_name().to_string(),
        program: driver.program().to_string(),
        installed,
        version,
        authenticated,
        detail: detail
            .or_else(|| (!installed).then(|| format!("{} is not on PATH", driver.program()))),
        models: driver.models().into_iter().map(|model| model.id).collect(),
    }
}

/// Run a probe command and return what it printed, or `None` if it could not
/// be run or said nothing useful.
///
/// Stderr is folded in because vendors report "not logged in" on either
/// stream, and a probe that ignored one would call a signed-out CLI unknown.
fn run(command: &CommandSpec) -> Option<String> {
    let mut process = std::process::Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(std::process::Stdio::null());
    crate::agent::sanitize(&mut process);
    for (key, value) in &command.env {
        process.env(key, value);
    }

    let output = with_timeout(process)?;
    let mut said = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if said.is_empty() {
        said = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    (!said.is_empty()).then_some(said)
}

/// Run a command, giving up after [`PROBE_TIMEOUT`].
///
/// A vendor CLI that waits for a terminal it does not have would otherwise
/// hold the daemon's request open forever.
fn with_timeout(mut process: std::process::Command) -> Option<std::process::Output> {
    let mut child = process
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
}

/// Ask every agent in a registry, in the order it offers them.
pub fn probe_all(registry: &super::Registry) -> Vec<AgentStatus> {
    registry
        .ids()
        .into_iter()
        .filter_map(|id| registry.get(id))
        .map(|driver| probe(driver.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::claude::ClaudeDriver;

    #[test]
    fn an_agent_that_is_not_installed_says_so_rather_than_looking_ready() {
        let driver = ClaudeDriver::with_program("/nonexistent/ginka-not-an-agent");
        let status = probe(&driver);
        assert!(!status.installed);
        assert!(!status.is_ready());
        assert_eq!(status.version, None);
        assert!(
            status.detail.unwrap().contains("not on PATH"),
            "the user has to be told what to install"
        );
    }

    #[test]
    fn a_binary_that_answers_is_installed_and_its_version_is_read() {
        // `echo` stands in for a vendor CLI: it exists and it prints.
        let driver = ClaudeDriver::with_program("/bin/echo");
        let status = probe(&driver);
        assert!(status.installed);
        assert_eq!(status.id, "claude");
        assert_eq!(status.program, "/bin/echo");
        // `echo --version` prints `--version`, which is not JSON, so the
        // sign-in question has no answer -- and that is not a `false`.
        assert_eq!(status.authenticated, None);
        assert!(
            status.is_ready(),
            "an agent that cannot be asked may still work"
        );
    }

    #[test]
    fn a_probe_that_hangs_is_given_up_on_rather_than_held_open() {
        let driver = ClaudeDriver::with_program("/bin/sleep");
        // The command is `sleep --version`, which exits immediately; what this
        // pins is that the runner returns at all.
        let started = std::time::Instant::now();
        let _ = probe(&driver);
        assert!(started.elapsed() < PROBE_TIMEOUT * 2);
    }
}
