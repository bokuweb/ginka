//! Where a provider's CLI is, and whether it works.
//!
//! Autodetection is a search path and nothing cleverer, because the answer has
//! to match what the user's own shell would find. When it does not — a version
//! manager, a nix profile, a checkout — the settings override is the way out,
//! and it wins outright (`docs/roadmap.md` §3.3 N14).

use ginka_protocol::provider::ProviderKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

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
    resolve_binary_in(provider, settings, &search_path())
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
    let Ok(mut child) = Command::new(binary)
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

fn search_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
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
