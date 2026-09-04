use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

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
            // Closed until they have something in them: the right panel's
            // surfaces and the terminal both land in M3, and two empty panels
            // either side of the conversation is a worse first impression than
            // a window that is only what works.
            right_panel_open: false,
            right_panel_width: 420.0,
            terminal_dock_open: false,
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
    /// How many checkpoints a workspace keeps.
    ///
    /// Each one holds a commit alive, so an old workspace accumulates objects
    /// git would otherwise collect. Rewinding is a thing done to recent work:
    /// past this many turns the reader is reading history, not undoing it.
    pub checkpoint_limit: u32,
    /// Per-agent overrides, keyed by driver id (`claude`, `codex`, …).
    ///
    /// A user whose agent CLI is version-managed, behind a wrapper script or
    /// pointed at a gateway configures it here; an id this build has no driver
    /// for is ignored rather than refused, so a settings file can outlive the
    /// build that reads it.
    pub agents: BTreeMap<String, AgentSettings>,
}

/// How to run one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSettings {
    /// The binary to run instead of the driver's default.
    pub program: Option<String>,
    /// Environment for the agent's process, on top of the inherited one.
    pub env: BTreeMap<String, String>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            port: 0,
            sync_interval_secs: 15,
            status_poll_secs: 60,
            retention_days: 30,
            checkpoint_limit: 200,
            agents: BTreeMap::new(),
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
    fn the_panels_that_have_nothing_in_them_start_closed() {
        let settings = AppSettings::default();
        assert!(settings.sidebar_open, "the workspace list is the way in");
        assert!(!settings.right_panel_open);
        assert!(!settings.terminal_dock_open);
        // Their sizes are remembered even while they are closed, so opening
        // one does not start from a default width.
        assert!(settings.right_panel_width > 0.);
        assert!(settings.terminal_dock_height > 0.);
    }

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
    fn an_agent_can_be_pointed_at_another_binary() {
        let settings: DaemonSettings = serde_json::from_str(
            r#"{"agents":{"claude":{"program":"/opt/homebrew/bin/claude",
                 "env":{"ANTHROPIC_BASE_URL":"http://localhost:8080"}}}}"#,
        )
        .expect("agent overrides parse");
        let claude = &settings.agents["claude"];
        assert_eq!(claude.program.as_deref(), Some("/opt/homebrew/bin/claude"));
        assert_eq!(
            claude.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("http://localhost:8080")
        );
    }

    #[test]
    fn a_settings_file_with_no_agents_still_loads() {
        let settings: DaemonSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.agents.is_empty());
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_ignored() {
        // A typo in a hand-edited settings file should be visible, not silently
        // dropped -- the fallback logs and reports it.
        let parsed = serde_json::from_str::<AppSettings>(r#"{"sidebarWidth": 300}"#);
        assert!(parsed.is_err());
    }
}
