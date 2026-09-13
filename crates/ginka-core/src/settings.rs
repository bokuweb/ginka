use anyhow::{Context, Result};
use ginka_protocol::provider::{ProviderKind, ProviderModel};
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
    /// Last explicitly selected model per provider, restored by the composer.
    pub recent_models: BTreeMap<String, String>,
    /// Last non-default effort and tier per provider/model pair.
    pub recent_model_options: BTreeMap<String, RecentModelOptions>,
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
            recent_models: BTreeMap::new(),
            recent_model_options: BTreeMap::new(),
        }
    }
}

impl AppSettings {
    /// Remember a model choice for the next conversation on `provider`.
    pub fn remember_model(&mut self, provider: impl Into<String>, model: impl Into<String>) {
        self.recent_models.insert(provider.into(), model.into());
    }

    /// Return a provider to choosing its own model.
    pub fn forget_model(&mut self, provider: &str) {
        self.recent_models.remove(provider);
    }

    /// The remembered model only while the provider still advertises it.
    ///
    /// Catalogues change independently of the app. Returning `None` for a
    /// removed id lets the provider choose its current default instead of
    /// repeatedly launching a model that no longer exists.
    pub fn recent_model<'a>(&'a self, provider: &str, models: &[ProviderModel]) -> Option<&'a str> {
        self.recent_models
            .get(provider)
            .filter(|recent| {
                models
                    .iter()
                    .any(|model| model.id.as_str() == recent.as_str())
            })
            .map(String::as_str)
    }

    /// Remember effort and tier for one provider/model pair.
    pub fn remember_model_options(
        &mut self,
        provider: &str,
        model: &str,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    ) {
        self.recent_model_options.insert(
            model_options_key(provider, model),
            RecentModelOptions {
                reasoning_effort,
                service_tier,
            },
        );
    }

    /// Recent options filtered against what this catalogue still accepts.
    pub fn recent_model_options(
        &self,
        provider: &str,
        model: &ProviderModel,
    ) -> RecentModelOptions {
        let Some(recent) = self
            .recent_model_options
            .get(&model_options_key(provider, &model.id))
        else {
            return RecentModelOptions::default();
        };
        RecentModelOptions {
            reasoning_effort: recent
                .reasoning_effort
                .as_ref()
                .filter(|effort| model.supports_reasoning_effort(effort))
                .cloned(),
            service_tier: recent
                .service_tier
                .as_ref()
                .filter(|tier| model.supports_service_tier(tier))
                .cloned(),
        }
    }
}

/// Recent non-default choices for one provider/model pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecentModelOptions {
    /// Last reasoning level selected for the model.
    pub reasoning_effort: Option<String>,
    /// Last service tier selected for the model.
    pub service_tier: Option<String>,
}

fn model_options_key(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
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
    /// Providers the user has switched off. Absent means enabled, so the file
    /// stays empty until someone actually turns something off.
    pub disabled_providers: Vec<ProviderKind>,
    /// Per-agent overrides, keyed by driver id (`claude`, `codex`, …).
    ///
    /// A user whose agent CLI is version-managed, behind a wrapper script or
    /// pointed at a gateway configures it here; an id this build has no driver
    /// for is ignored rather than refused, so a settings file can outlive the
    /// build that reads it.
    pub agents: BTreeMap<String, AgentSettings>,
    /// Logins beyond each provider's own, keyed by account id
    /// (`docs/accounts.md` §3). A provider's default account is never here:
    /// it is the vendor's own home, configured by `agents` above.
    pub accounts: BTreeMap<String, AccountSettings>,
    /// Chat connectors: which platforms the daemon listens to, in which
    /// channels, and who may speak (`docs/connectors.md` §4.2). Tokens are
    /// never here.
    pub connectors: crate::connector::ConnectorsSettings,
    /// The MCP servers every agent is given when it starts
    /// (`crate::tools`): Ginka's own bridge, zvec-grep where a workspace is
    /// indexed, and the user's own.
    pub tools: crate::tools::ToolSettings,
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

    /// Where a provider's CLI actually is, when autodetection cannot find it.
    ///
    /// Agent CLIs are installed through version managers, nix profiles and
    /// plain checkouts; without this, a failed probe is a dead end with no way
    /// out from inside the app (`docs/roadmap.md` §3.3 N14).
    pub fn binary_override(&self, provider: ProviderKind) -> Option<&Path> {
        self.agents
            .get(provider.as_str())
            .and_then(|agent| agent.program.as_deref())
            .map(Path::new)
    }

    /// `None` clears the override and returns the provider to autodetection.
    pub fn set_binary_override(&mut self, provider: ProviderKind, path: Option<PathBuf>) {
        let entry = self
            .agents
            .entry(provider.as_str().to_string())
            .or_default();
        entry.program = path.map(|path| path.to_string_lossy().into_owned());
        if entry.program.is_none() && entry.env.is_empty() {
            self.agents.remove(provider.as_str());
        }
    }
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

/// One login of one provider, beyond the provider's own.
///
/// The directory it names is `~/.ginka/accounts/<id>/`, derived from the id
/// rather than stored, so the record cannot point somewhere it does not own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSettings {
    pub provider: ProviderKind,
    /// What the chip says.
    pub label: String,
    /// Environment for this account's processes, applied after the
    /// provider's own and before the session's. The same escape hatch as
    /// [`AgentSettings::env`], and where a key goes if the user wants one
    /// attached to one login rather than to every session of a provider.
    #[serde(default)]
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
            disabled_providers: Vec::new(),
            agents: BTreeMap::new(),
            accounts: BTreeMap::new(),
            connectors: crate::connector::ConnectorsSettings::default(),
            tools: crate::tools::ToolSettings::default(),
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
    fn the_recent_model_is_kept_per_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.json");
        let mut settings = AppSettings::default();
        settings.remember_model("codex", "gpt-next");
        settings.remember_model_options(
            "codex",
            "gpt-next",
            Some("high".into()),
            Some("priority".into()),
        );
        save(&path, &settings).unwrap();

        let loaded: AppSettings = load(&path);
        assert_eq!(loaded.recent_models.get("codex"), Some(&"gpt-next".into()));
        assert_eq!(loaded.recent_models.get("claude"), None);
        assert_eq!(
            loaded.recent_model("codex", &[ProviderModel::new("gpt-next", "Next")]),
            Some("gpt-next")
        );
        assert_eq!(
            loaded.recent_model("codex", &[ProviderModel::new("gpt-newer", "Newer")]),
            None,
            "a stale id returns to the provider default"
        );
        let model = ProviderModel::new("gpt-next", "Next").with_reasoning_efforts([
            ginka_protocol::provider::ProviderOption::new("high", "High"),
        ]);
        let options = loaded.recent_model_options("codex", &model);
        assert_eq!(options.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(options.service_tier, None, "removed options are ignored");
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
    fn an_account_is_a_provider_a_label_and_optionally_its_own_variables() {
        let settings: DaemonSettings = serde_json::from_str(
            r#"{"accounts":{"claude-work":{"provider":"claude","label":"Work",
                 "env":{"ANTHROPIC_BASE_URL":"https://gateway"}},
                 "codex-personal":{"provider":"codex","label":"Personal"}}}"#,
        )
        .expect("accounts parse");
        let work = &settings.accounts["claude-work"];
        assert_eq!(work.provider, ProviderKind::Claude);
        assert_eq!(work.label, "Work");
        assert_eq!(
            work.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://gateway")
        );
        assert!(settings.accounts["codex-personal"].env.is_empty());
        // A record cannot say where its directory is: that is derived.
        assert!(
            serde_json::from_str::<DaemonSettings>(
                r#"{"accounts":{"x":{"provider":"claude","label":"X","home":"/elsewhere"}}}"#
            )
            .is_err()
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
