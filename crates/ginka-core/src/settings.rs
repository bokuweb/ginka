use anyhow::{Context, Result};
use ginka_protocol::provider::ProviderKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Settings the UI owns: `~/.ginka/app.json`.
///
/// Split from the daemon's settings deliberately — the daemon runs headless in
/// CI and on machines with no display, and must not carry window geometry or a
/// theme choice around with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    pub appearance: Appearance,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub right_panel_open: bool,
    pub right_panel_width: f32,
    pub terminal_dock_open: bool,
    pub terminal_dock_height: f32,
    /// Restored on launch when the workspace still exists.
    pub last_workspace: Option<String>,
    /// BCP-47 tag; `None` follows the system locale.
    pub locale: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            // The defaults in docs/ui.md §2.
            sidebar_open: true,
            sidebar_width: 250.0,
            right_panel_open: true,
            right_panel_width: 420.0,
            terminal_dock_open: true,
            terminal_dock_height: 220.0,
            last_workspace: None,
            locale: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    Dark,
    #[default]
    System,
}

/// Settings the daemon owns: `~/.ginka/settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonSettings {
    /// 0 asks the OS for a free port; the chosen one is published in
    /// `daemon.json` for the app and CLI to find.
    pub port: u16,
    /// How often worktrees are reconciled against git, in seconds.
    pub sync_interval_secs: u64,
    /// How often branch/CI status is polled, in seconds.
    pub status_poll_secs: u64,
    /// Days of task and usage history to keep.
    pub retention_days: u32,
    /// Providers the user has switched off. Absent means enabled, so the file
    /// stays empty until someone actually turns something off.
    pub disabled_providers: Vec<ProviderKind>,
    /// Where a provider's CLI actually is, when autodetection cannot find it.
    ///
    /// Agent CLIs are installed through version managers, nix profiles and
    /// plain checkouts; without this, a failed probe is a dead end with no way
    /// out from inside the app. See `docs/roadmap.md` §3.3 N14.
    pub provider_binaries: BTreeMap<ProviderKind, PathBuf>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            port: 0,
            sync_interval_secs: 15,
            status_poll_secs: 60,
            retention_days: 30,
            disabled_providers: Vec::new(),
            provider_binaries: BTreeMap::new(),
        }
    }
}

impl DaemonSettings {
    pub fn is_enabled(&self, provider: ProviderKind) -> bool {
        !self.disabled_providers.contains(&provider)
    }

    /// Enable or disable a provider. Re-enabling removes the entry rather than
    /// leaving a tombstone, so the file only ever records real choices.
    pub fn set_enabled(&mut self, provider: ProviderKind, enabled: bool) {
        if enabled {
            self.disabled_providers.retain(|kind| *kind != provider);
        } else if !self.disabled_providers.contains(&provider) {
            self.disabled_providers.push(provider);
        }
    }

    pub fn binary_override(&self, provider: ProviderKind) -> Option<&Path> {
        self.provider_binaries.get(&provider).map(PathBuf::as_path)
    }

    /// `None` clears the override and returns the provider to autodetection.
    pub fn set_binary_override(&mut self, provider: ProviderKind, path: Option<PathBuf>) {
        match path {
            Some(path) => {
                self.provider_binaries.insert(provider, path);
            }
            None => {
                self.provider_binaries.remove(&provider);
            }
        }
    }
}

/// Read a settings file, falling back to defaults.
///
/// A missing file is normal (first run). A *corrupt* file is not silently
/// replaced: we log loudly and use defaults for this session, so a syntax error
/// in a hand-edited file never destroys the rest of the user's configuration.
pub fn load<T: Default + serde::de::DeserializeOwned>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    path = %path.display(),
                    %error,
                    "settings file is not valid; using defaults for this session without \
                     overwriting the file"
                );
                T::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "could not read settings");
            T::default()
        }
    }
}

/// Write settings atomically, so a crash mid-write cannot truncate the file.
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded: AppSettings = load(&tmp.path().join("absent.json"));
        assert_eq!(loaded, AppSettings::default());
    }

    #[test]
    fn round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.json");
        let settings = AppSettings {
            appearance: Appearance::Dark,
            sidebar_width: 310.0,
            ..AppSettings::default()
        };
        save(&path, &settings).unwrap();
        assert_eq!(load::<AppSettings>(&path), settings);
    }

    #[test]
    fn corrupt_file_falls_back_without_being_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load::<AppSettings>(&path), AppSettings::default());
        // The user's file is still theirs to fix.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_ignored() {
        // A typo in a hand-edited settings file should be visible, not silently
        // dropped -- the fallback logs and reports it.
        let parsed = serde_json::from_str::<AppSettings>(r#"{"sidebarWidth": 300}"#);
        assert!(parsed.is_err());
    }
}
