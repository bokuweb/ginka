//! What the window remembers: `~/.e1/app.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The window's own settings.
///
/// `deny_unknown_fields` so a typo in a hand-edited file is reported rather
/// than silently ignored, and `default` so a file from an older build still
/// loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    /// Light or dark; `None` until the reader picks one, which means the
    /// window takes the OS's answer and follows it.
    #[serde(deserialize_with = "chosen")]
    pub appearance: Option<Appearance>,
    /// Which agent CLI an ask goes to, by [`crate::agents::Kind::id`].
    /// `None` until one is picked, when the first one found is used.
    #[serde(default)]
    pub agent: Option<String>,
    /// Which model each CLI is asked with, by [`crate::agents::Kind::id`]
    /// and then whatever that CLI calls the model (`opus`, `gpt-5.6-sol`).
    /// Per CLI because the names are: one field would be a name that means
    /// nothing to the next one. A CLI with no entry is started with no
    /// model on its command line, which leaves its own configuration
    /// alone. Free-form, so a name written here by hand is passed through
    /// whether this build has heard of it or not.
    #[serde(default)]
    pub agent_models: BTreeMap<String, String>,
    /// How much thinking each CLI is asked for, the same way (`high`,
    /// `xhigh`). Only the CLIs that take one have an entry.
    #[serde(default)]
    pub agent_efforts: BTreeMap<String, String>,
    /// Whether the navigation column is showing.
    pub sidebar_open: bool,
    /// Its width, kept while it is closed.
    pub sidebar_width: f32,
    /// Whether the detail column is showing.
    pub right_panel_open: bool,
    /// Its width, kept while it is closed.
    pub right_panel_width: f32,
    /// Whether the CLI-backed chat column is showing.
    pub agent_panel_open: bool,
    /// Its width, kept while it is closed.
    pub agent_panel_width: f32,
    /// BCP-47 tag; `None` follows the system locale.
    pub locale: Option<String>,
    /// The repository that was on screen, as `owner/name`, restored on launch.
    pub last_repo: Option<String>,
    /// The owners whose repositories are folded away in the sidebar.
    pub collapsed_owners: Vec<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            appearance: None,
            agent: None,
            agent_models: BTreeMap::new(),
            agent_efforts: BTreeMap::new(),
            // The defaults in docs/ui.md §2. Unlike Ginka, the right panel
            // starts open: it is the reading pane, and there is something to
            // read the moment a row is picked.
            sidebar_open: true,
            sidebar_width: 250.0,
            right_panel_open: true,
            right_panel_width: 420.0,
            agent_panel_open: false,
            agent_panel_width: 420.0,
            locale: None,
            last_repo: None,
            collapsed_owners: Vec::new(),
        }
    }
}

/// The theme choice: one of two, or none until the reader makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Light.
    Light,
    /// Dark.
    Dark,
}

/// What an older settings file may hold where the choice goes.
///
/// The control used to have a third state, `system`, which deferred to the
/// OS. There are two now, and a file that still says `system` — or anything
/// else unreadable — reads as no choice at all, which behaves the way that
/// third state did until the reader picks: the window opens on whatever the
/// OS is showing, and follows it while nothing has been chosen.
fn chosen<'de, D>(deserializer: D) -> Result<Option<Appearance>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(match raw.as_deref() {
        Some("light") => Some(Appearance::Light),
        Some("dark") => Some(Appearance::Dark),
        _ => None,
    })
}

/// Read a settings file, falling back to defaults.
///
/// A missing file is normal (first run). A *corrupt* file is not silently
/// replaced: we log loudly and use defaults for this session, so a syntax
/// error in a hand-edited file never destroys the rest of the configuration.
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
    fn a_settings_file_from_when_there_were_three_appearances_still_loads() {
        let older: AppSettings =
            serde_json::from_str(r#"{"appearance":"system","sidebar_width":300.0}"#).unwrap();
        // The third state is gone; what is left of it is "no choice yet",
        // and the rest of the file survives rather than being reset.
        assert_eq!(older.appearance, None);
        assert_eq!(older.sidebar_width, 300.0);
        let chosen: AppSettings = serde_json::from_str(r#"{"appearance":"light"}"#).unwrap();
        assert_eq!(chosen.appearance, Some(Appearance::Light));
    }

    #[test]
    fn the_reading_pane_starts_open() {
        let settings = AppSettings::default();
        assert!(settings.sidebar_open);
        assert!(settings.right_panel_open);
    }

    #[test]
    fn a_file_from_an_older_build_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.json");
        std::fs::write(&path, r#"{"sidebar_open": false}"#).unwrap();
        let settings: AppSettings = load(&path);
        assert!(!settings.sidebar_open);
        assert_eq!(settings.right_panel_width, 420.0);
    }

    #[test]
    fn a_corrupt_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.json");
        std::fs::write(&path, "{ not json").unwrap();
        let settings: AppSettings = load(&path);
        assert_eq!(settings, AppSettings::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.json");
        let settings = AppSettings {
            last_repo: Some("bokuweb/e1".into()),
            sidebar_width: 300.0,
            agent: Some("codex".into()),
            agent_models: BTreeMap::from([("codex".to_string(), "gpt-5.6-sol".to_string())]),
            agent_efforts: BTreeMap::from([("codex".to_string(), "high".to_string())]),
            ..AppSettings::default()
        };
        save(&path, &settings).unwrap();
        assert_eq!(load::<AppSettings>(&path), settings);
    }
}
