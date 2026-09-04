//! N14: providers are configurable — disabled individually, and their binary
//! path overridable when autodetection cannot find them.

use ginka_core::settings::{self, DaemonSettings};
use ginka_protocol::provider::ProviderKind;
use std::path::{Path, PathBuf};

#[test]
fn every_provider_is_enabled_by_default() {
    let settings = DaemonSettings::default();
    for kind in ProviderKind::ALL {
        assert!(settings.is_enabled(kind));
        assert_eq!(settings.binary_override(kind), None);
    }
}

#[test]
fn a_disabled_provider_stays_disabled_across_a_save_and_load() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");

    let mut written = DaemonSettings::default();
    written.set_enabled(ProviderKind::Codex, false);
    written.set_binary_override(ProviderKind::Claude, Some(PathBuf::from("/opt/bin/claude")));
    settings::save(&path, &written).unwrap();

    let read: DaemonSettings = settings::load(&path);
    assert!(!read.is_enabled(ProviderKind::Codex));
    assert!(read.is_enabled(ProviderKind::Claude));
    assert_eq!(
        read.binary_override(ProviderKind::Claude),
        Some(Path::new("/opt/bin/claude"))
    );
}

#[test]
fn re_enabling_removes_the_entry_rather_than_leaving_a_tombstone() {
    let mut settings = DaemonSettings::default();
    settings.set_enabled(ProviderKind::Amp, false);
    settings.set_enabled(ProviderKind::Amp, true);
    assert_eq!(settings, DaemonSettings::default());
}

#[test]
fn clearing_an_override_falls_back_to_autodetection() {
    let mut settings = DaemonSettings::default();
    settings.set_binary_override(ProviderKind::Cursor, Some(PathBuf::from("/tmp/cursor")));
    settings.set_binary_override(ProviderKind::Cursor, None);
    assert_eq!(settings.binary_override(ProviderKind::Cursor), None);
}

#[test]
fn disabling_a_provider_twice_does_not_duplicate_it() {
    let mut settings = DaemonSettings::default();
    settings.set_enabled(ProviderKind::Codex, false);
    settings.set_enabled(ProviderKind::Codex, false);
    assert_eq!(settings.disabled_providers.len(), 1);
}
