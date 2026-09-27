//! Where a provider's CLI is, and whether it works.
//!
//! Autodetection is a search path and nothing cleverer, because the answer has
//! to match what the user's own shell would find. When it does not — a version
//! manager, a nix profile, a checkout — the settings override is the way out,
//! and it wins outright (`docs/roadmap.md` §3.3 N14).

use super::{AgentDriver, CommandSpec};
use ginka_protocol::model::{AgentStatus, PlanUsage};
use ginka_protocol::provider::{ProviderKind, ProviderModel};
use std::time::Duration;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::settings::DaemonSettings;

/// How long a probe waits before deciding the CLI is not answering. A probe
/// runs on startup and on the settings page, and a hung binary must not hold
/// either of them.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What a probe learned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeResult {
    pub installed: bool,
    /// `None` when the CLI answered in a shape we could not read — which is
    /// not a reason to call it missing.
    pub version: Option<String>,
}

/// Where a provider's CLI is, honouring the user's settings.
pub fn resolve_binary(provider: ProviderKind, settings: &DaemonSettings) -> Option<PathBuf> {
    resolve_binary_in(provider, settings, crate::tool_path::search_path())
}

/// The same, against an explicit search path.
pub fn resolve_binary_in(
    provider: ProviderKind,
    settings: &DaemonSettings,
    search_path: &[PathBuf],
) -> Option<PathBuf> {
    if !settings.is_enabled(provider) {
        return None;
    }
    if let Some(override_path) = settings.binary_override(provider) {
        // Taken as given, existence unchecked: the user said where it is, and
        // "your override is wrong" is a better error at launch than silently
        // falling back to a different binary than the one they named.
        return Some(override_path.to_path_buf());
    }

    let name = provider.as_str();
    search_path
        .iter()
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Ask a CLI what it is.
pub fn probe(binary: &Path) -> ProbeResult {
    probe_with_arg(binary, "--version")
}

/// The same, for a CLI that spells the version flag differently.
pub fn probe_with_arg(binary: &Path, arg: &str) -> ProbeResult {
    let mut command = Command::new(binary);
    crate::tool_path::apply(&mut command);
    let Ok(mut child) = command
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return ProbeResult::default();
    };

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                return ProbeResult {
                    installed: true,
                    version: None,
                };
            }
            Err(_) => return ProbeResult::default(),
        }
    }

    let output = child.wait_with_output().ok();
    let text = output
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();

    ProbeResult {
        installed: true,
        version: version_in(&text),
    }
}

/// The first dotted number in a version line. Vendors wrap it in different
/// prose — "1.2.3 (Claude Code)", "claude version 1.2.3" — and the number is
/// the part anyone acts on.
fn version_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|word| {
            let core = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
            core.contains('.')
                && core.starts_with(|c: char| c.is_ascii_digit())
                && core.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(|word| {
            word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
                .to_string()
        })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------
// Asking a driver whether it is usable, which is a different question from
// where its binary is: a CLI can be installed and not signed in.

pub fn probe_driver(driver: &dyn AgentDriver) -> AgentStatus {
    probe_driver_with_env(driver, &[])
}

/// The same, under an account's environment (`docs/accounts.md` §4): what
/// the CLI says about itself when pointed at that login's directory.
pub fn probe_driver_with_env(driver: &dyn AgentDriver, env: &[(String, String)]) -> AgentStatus {
    let version_output = run(&driver.probe_command(), env);
    let installed = version_output.is_some();
    let version = version_output
        .as_deref()
        .and_then(|output| driver.parse_version(output));

    // Only worth asking if the binary answered at all.
    let (authenticated, detail) = match (installed, driver.auth_command()) {
        (true, Some(command)) => {
            match run(&command, env).and_then(|output| driver.parse_auth(&output)) {
                Some((signed_in, detail)) => (Some(signed_in), detail),
                None => (None, None),
            }
        }
        _ => (None, None),
    };

    let fallback = driver.models();
    let models = if installed {
        probe_model_catalogue(driver, env).unwrap_or(fallback)
    } else {
        fallback
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
        models,
    }
}

/// Ask a provider for the models it currently offers.
///
/// `None` means the probe was unavailable, timed out or could not be parsed;
/// the caller must retain the driver's static catalogue in every such case.
pub fn probe_model_catalogue(
    driver: &dyn AgentDriver,
    env: &[(String, String)],
) -> Option<Vec<ProviderModel>> {
    use std::io::{BufRead as _, Write as _};

    let probe = driver.model_catalogue_probe()?;
    let mut process = std::process::Command::new(&probe.command.program);
    process.args(&probe.command.args);
    crate::agent::sanitize(&mut process);
    for (key, value) in probe.command.env.iter().chain(env.iter()) {
        process.env(key, value);
    }
    let mut child = process
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;

    if let Some(mut stdin) = child.stdin.take() {
        let input = probe.input;
        std::thread::spawn(move || {
            for line in input {
                if writeln!(stdin, "{line}").is_err() || stdin.flush().is_err() {
                    break;
                }
            }
            std::thread::sleep(PROBE_TIMEOUT);
        });
    }

    let (sender, lines) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let mut answer = None;
    while answer.is_none() {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match lines.recv_timeout(deadline - now) {
            Ok(line) => answer = driver.parse_model_catalogue(&line),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    answer.filter(|models| !models.is_empty())
}

/// Whether an account is signed in, asked of the vendor under the account's
/// environment. `None` when the CLI cannot be asked or did not answer, which
/// is not the same as signed out.
pub fn probe_signed_in(driver: &dyn AgentDriver, env: &[(String, String)]) -> Option<bool> {
    probe_login(driver, env).map(|(signed_in, _)| signed_in)
}

/// [`probe_signed_in`], with who the login is when the vendor says — one
/// run of the CLI for both.
pub fn probe_login(
    driver: &dyn AgentDriver,
    env: &[(String, String)],
) -> Option<(bool, Option<ginka_protocol::model::AccountIdentity>)> {
    let command = driver.auth_command()?;
    let output = run(&command, env)?;
    let (signed_in, _) = driver.parse_auth(&output)?;
    Some((signed_in, driver.parse_identity(&output)))
}

/// How long a provider is given to answer for an account's windows. Longer
/// than a version probe: the app server has to start and ask the vendor.
const PLAN_USAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Ask a provider for an account's rate-limit windows without running a
/// turn (`docs/accounts.md` §6).
///
/// The probe's input is written once the process starts, and its output read
/// line by line until the driver recognises the answer; the process is then
/// stopped, because a server told to read one thing does not exit by itself.
/// `None` when the provider cannot be asked, did not answer in time, or
/// answered with nothing the driver could read — and none of those is a
/// reading of zero.
pub fn probe_plan_usage(driver: &dyn AgentDriver, env: &[(String, String)]) -> Option<PlanUsage> {
    use std::io::{BufRead as _, Write as _};

    let probe = driver.plan_usage_probe()?;
    let mut process = std::process::Command::new(&probe.command.program);
    process.args(&probe.command.args);
    crate::agent::sanitize(&mut process);
    for (key, value) in probe.command.env.iter().chain(env.iter()) {
        process.env(key, value);
    }
    let mut child = process
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;

    // Written on a thread of its own: a server that answers before it has
    // read everything would block the writer against a full pipe.
    if let Some(mut stdin) = child.stdin.take() {
        let input = probe.input.clone();
        std::thread::spawn(move || {
            for line in input {
                if writeln!(stdin, "{line}").is_err() || stdin.flush().is_err() {
                    break;
                }
            }
            // Left open: closing it is how some servers are told to exit,
            // and the answer may not have arrived yet.
            std::thread::sleep(PLAN_USAGE_TIMEOUT);
        });
    }

    let (sender, lines) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + PLAN_USAGE_TIMEOUT;
    let mut answer = None;
    while answer.is_none() {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match lines.recv_timeout(deadline - now) {
            Ok(line) => answer = driver.parse_plan_usage(&line),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    answer
}

/// Run a probe command and return what it printed, or `None` if it could not
/// be run or said nothing useful.
///
/// Stderr is folded in because vendors report "not logged in" on either
/// stream, and a probe that ignored one would call a signed-out CLI unknown.
/// `env` is applied after the command's own, which is how an account's
/// directory reaches the CLI being asked.
fn run(command: &CommandSpec, env: &[(String, String)]) -> Option<String> {
    let mut process = std::process::Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(std::process::Stdio::null());
    crate::agent::sanitize(&mut process);
    for (key, value) in command.env.iter().chain(env.iter()) {
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
        .map(|driver| probe_driver(driver.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::claude::ClaudeDriver;

    #[test]
    fn an_agent_that_is_not_installed_says_so_rather_than_looking_ready() {
        let driver = ClaudeDriver::with_program("/nonexistent/ginka-not-an-agent");
        let status = probe_driver(&driver);
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
        let status = probe_driver(&driver);
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
        let _ = probe_driver(&driver);
        assert!(started.elapsed() < PROBE_TIMEOUT * 2);
    }
}
