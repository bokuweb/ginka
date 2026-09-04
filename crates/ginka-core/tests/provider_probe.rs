//! Finding a provider's CLI, and asking it whether it is usable.

use ginka_core::driver::{ProbeResult, probe, probe_with_arg, resolve_binary_in};
use ginka_core::settings::DaemonSettings;
use ginka_protocol::provider::ProviderKind;
use std::path::{Path, PathBuf};

const FAKE_AGENT: &str = env!("CARGO_BIN_EXE_ginka-fake-agent");

/// A directory holding one executable under the given name.
fn bin_dir(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::copy(FAKE_AGENT, &path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

#[test]
fn a_cli_on_the_search_path_is_found() {
    let dir = bin_dir("claude");
    let found = resolve_binary_in(
        ProviderKind::Claude,
        &DaemonSettings::default(),
        &[dir.path().to_path_buf()],
    );
    assert_eq!(found, Some(dir.path().join("claude")));
}

#[test]
fn a_provider_that_is_not_installed_is_simply_absent() {
    let dir = bin_dir("claude");
    assert_eq!(
        resolve_binary_in(
            ProviderKind::Amp,
            &DaemonSettings::default(),
            &[dir.path().to_path_buf()]
        ),
        None
    );
}

#[test]
fn an_override_wins_over_the_search_path() {
    // The way out when autodetection cannot see a CLI installed by a version
    // manager, a nix profile or a checkout.
    let dir = bin_dir("claude");
    let mut settings = DaemonSettings::default();
    settings.set_binary_override(
        ProviderKind::Claude,
        Some(PathBuf::from("/opt/custom/claude")),
    );

    assert_eq!(
        resolve_binary_in(ProviderKind::Claude, &settings, &[dir.path().to_path_buf()]),
        Some(PathBuf::from("/opt/custom/claude"))
    );
}

#[test]
fn a_disabled_provider_is_not_resolved_however_installed_it_is() {
    let dir = bin_dir("claude");
    let mut settings = DaemonSettings::default();
    settings.set_enabled(ProviderKind::Claude, false);

    assert_eq!(
        resolve_binary_in(ProviderKind::Claude, &settings, &[dir.path().to_path_buf()]),
        None
    );
}

#[test]
fn a_directory_named_like_the_cli_is_not_mistaken_for_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("claude")).unwrap();
    assert_eq!(
        resolve_binary_in(
            ProviderKind::Claude,
            &DaemonSettings::default(),
            &[dir.path().to_path_buf()]
        ),
        None
    );
}

#[test]
fn the_first_directory_on_the_path_wins() {
    let first = bin_dir("claude");
    let second = bin_dir("claude");
    assert_eq!(
        resolve_binary_in(
            ProviderKind::Claude,
            &DaemonSettings::default(),
            &[first.path().to_path_buf(), second.path().to_path_buf()]
        ),
        Some(first.path().join("claude"))
    );
}

#[test]
fn a_probe_reports_the_version_the_cli_gives() {
    let result = probe(Path::new(FAKE_AGENT));
    assert!(result.installed, "{result:?}");
    assert_eq!(result.version.as_deref(), Some("9.9.9"));
}

#[test]
fn a_binary_that_is_not_there_probes_as_not_installed() {
    let result = probe(Path::new("ginka-no-such-agent-binary"));
    assert_eq!(
        result,
        ProbeResult {
            installed: false,
            version: None,
        }
    );
}

#[test]
fn a_cli_that_answers_with_something_unexpected_is_still_installed() {
    // Version strings are the vendor's business and change shape; failing to
    // read one is not a reason to call the CLI missing.
    let result = probe_with_arg(Path::new(FAKE_AGENT), "--silent-version");
    assert!(result.installed);
    assert_eq!(result.version, None);
}
