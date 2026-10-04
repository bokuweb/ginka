//! The vocabulary a session is configured with: which agent runs it, which
//! model and effort it runs at, and what it is allowed to touch.
//!
//! Model, reasoning effort and service tier are per-provider strings rather
//! than enums on purpose — they are the vendor's vocabulary, discovered from
//! the CLI at runtime (`docs/roadmap.md` §3.3 N3), and an enum here would be
//! stale the week after a vendor ships. Only `AccessMode`, which is ours, is
//! closed.

use serde::{Deserialize, Serialize};

/// The agent CLIs Ginka knows how to drive.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Anthropic's Claude Code CLI.
    Claude,
    /// OpenAI's Codex CLI.
    Codex,
    /// The Amp CLI.
    Amp,
    /// The OpenCode CLI.
    OpenCode,
    /// Cursor's command-line agent.
    Cursor,
    /// Google's Gemini CLI.
    Gemini,
}

impl ProviderKind {
    /// Every provider, in the order pickers and settings list them.
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Codex,
        Self::Amp,
        Self::OpenCode,
        Self::Cursor,
        Self::Gemini,
    ];

    /// The stable lowercase id, as serialized and as a flag takes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Amp => "amp",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
        }
    }

    /// Read [`ProviderKind::as_str`] back; `None` for an id this build does not know.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a session is allowed to do without asking. Ours, not a vendor's: every
/// driver maps it onto whatever its CLI calls the same idea.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// Read and reason, but every write and command needs approval.
    ReadOnly,
    /// Edit freely inside the worktree; commands are approved.
    #[default]
    Ask,
    /// Edit and run without approval.
    Auto,
}

impl AccessMode {
    /// The stable word for each mode: what a column stores and a flag takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    /// Whether this mode allows strictly less than `other`: read-only is
    /// narrower than ask, which is narrower than auto. A turn may run
    /// narrower than its conversation; never wider (N2).
    pub fn is_narrower_than(self, other: Self) -> bool {
        let rank = |mode: Self| match mode {
            Self::ReadOnly => 0,
            Self::Ask => 1,
            Self::Auto => 2,
        };
        rank(self) < rank(other)
    }

    /// Read [`AccessMode::as_str`] back, with the underscore spelling and the
    /// vendors' nearest words accepted too: a flag typed by hand should not
    /// fail on a hyphen.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "read-only" | "read_only" | "readonly" | "plan" => Some(Self::ReadOnly),
            "ask" | "default" | "edit" => Some(Self::Ask),
            "auto" | "full" | "bypass" => Some(Self::Auto),
            _ => None,
        }
    }

    /// Every mode, in the order a picker lists them: least to most.
    pub const ALL: [AccessMode; 3] = [Self::ReadOnly, Self::Ask, Self::Auto];
}

/// One selectable value inside a model — an effort level, a service tier.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOption {
    /// The value passed to the provider.
    pub id: String,
    /// The human-readable picker label.
    pub label: String,
}

impl ProviderOption {
    /// An option with its provider value and picker label.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// One model a provider offers, with the options that apply to it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModel {
    /// The model id passed to the provider.
    pub id: String,
    /// The human-readable picker label.
    pub label: String,
    /// The provider's own default. At most one model in a catalogue sets it;
    /// `default_of` falls back to the first when none does.
    pub is_default: bool,
    /// Reasoning levels accepted by this model, in picker order.
    pub reasoning_efforts: Vec<ProviderOption>,
    /// Service tiers accepted by this model, in picker order.
    pub service_tiers: Vec<ProviderOption>,
}

impl ProviderModel {
    /// A model that is not the default and accepts no options.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            is_default: false,
            reasoning_efforts: Vec::new(),
            service_tiers: Vec::new(),
        }
    }

    /// Mark this model as the provider's default.
    #[must_use]
    pub fn as_default(mut self) -> Self {
        self.is_default = true;
        self
    }

    /// Replace the reasoning levels this model accepts.
    #[must_use]
    pub fn with_reasoning_efforts(
        mut self,
        options: impl IntoIterator<Item = ProviderOption>,
    ) -> Self {
        self.reasoning_efforts = options.into_iter().collect();
        self
    }

    /// Replace the service tiers this model accepts.
    #[must_use]
    pub fn with_service_tiers(mut self, options: impl IntoIterator<Item = ProviderOption>) -> Self {
        self.service_tiers = options.into_iter().collect();
        self
    }

    /// Whether `id` is one of this model's reasoning levels.
    pub fn supports_reasoning_effort(&self, id: &str) -> bool {
        self.reasoning_efforts.iter().any(|option| option.id == id)
    }

    /// Whether `id` is one of this model's service tiers.
    pub fn supports_service_tier(&self, id: &str) -> bool {
        self.service_tiers.iter().any(|option| option.id == id)
    }

    /// The model to start on: the marked default, else the first offered.
    pub fn default_of(models: &[Self]) -> Option<&Self> {
        models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| models.first())
    }
}

/// Everything about a session that the user can change while it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionOptions {
    /// Provider model id; `None` uses the provider's default.
    pub model: Option<String>,
    /// Provider reasoning level; `None` uses the model's default.
    pub reasoning_effort: Option<String>,
    /// Provider service tier; `None` uses the model's default.
    pub service_tier: Option<String>,
    /// What the agent may do without asking.
    pub access_mode: AccessMode,
    /// Which login the session runs on; `None` asks the caller to resolve the
    /// provider's active account (`docs/accounts.md` §5).
    pub account: Option<crate::ids::AccountId>,
}

impl SessionOptions {
    /// Whether applying `other` would change anything at all.
    pub fn differs_from(&self, other: &Self) -> bool {
        self != other
    }

    /// Whether the session must be restarted whatever the transport claims.
    ///
    /// Model, effort and tier are the driver's call: most transports carry
    /// them on the next turn. Access mode is not — loosening or tightening
    /// what an agent that is *already running* may touch deserves a fresh
    /// session even where the transport would happily accept the change.
    /// Neither is the account: the vendor's thread lives in the account's
    /// directory, and a resume cannot cross directories (`docs/accounts.md`
    /// §5).
    pub fn forces_restart(before: &Self, after: &Self) -> bool {
        before.access_mode != after.access_mode || before.account != after.account
    }
}

/// A driver's answer to "can you apply this without being restarted?".
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionOutcome {
    /// The running session took the change.
    Absorbed,
    /// The session has to be torn down and started again to apply it.
    RestartRequired,
}

impl OptionOutcome {
    /// Whether the running session took the change without a restart.
    pub fn absorbed(self) -> bool {
        matches!(self, Self::Absorbed)
    }
}
