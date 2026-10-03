//! The domain objects as they appear on the wire.
//!
//! These live in the protocol crate rather than in `ginka-core` because the
//! CLI, the app and any future non-Rust client need to name them without
//! linking the daemon's domain logic. `ginka-core` persists them; nothing here
//! knows what a database or a git repository is.

use crate::ids::{AccountId, CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
use crate::provider::{AccessMode, ProviderKind};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A file the user attached, as a client sees it.
///
/// Deliberately without a path: the bytes live on the daemon's host, and a
/// client that is not on that host has no business interpreting one
/// (`docs/roadmap.md` §4.1). What a message carries is the reference, and the
/// daemon is what turns it back into a file for the agent.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// `ginka-attachment:<id>` — what a message refers to it by.
    pub reference: String,
    /// The name the user knows it by. Display only: never a path component.
    pub name: String,
    /// How much was stored, so a client can show it without reading it back.
    pub bytes: usize,
}

/// A registered repository or folder.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Stable key, slugified from the directory name.
    pub name: ProjectName,
    /// The repository's top level, not whatever subdirectory was registered.
    pub path: PathBuf,
    /// What new worktrees branch from. Empty for a `plain` project.
    pub default_branch: String,
    /// Free-form grouping label shown in the sidebar.
    pub label: Option<String>,
    /// Ascending display order within the sidebar.
    pub sort_order: i64,
    pub kind: ProjectKind,
    /// `None` means "not probed yet", and is treated as `true` so the first CI
    /// poll after a cold boot still runs.
    pub has_origin: Option<bool>,
}

/// Whether a project has git features at all.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    /// Worktree per workspace, branches, PR/CI features.
    Git,
    /// A plain folder: one implicit workspace, git features off.
    Plain,
}

impl ProjectKind {
    /// The value stored in SQLite and printed by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Plain => "plain",
        }
    }

    /// Parse a stored value, defaulting to `git`.
    ///
    /// A row written by a newer build with a kind this one does not know is
    /// still more useful as a git project than as a hard error on startup.
    pub fn parse(value: &str) -> Self {
        match value {
            "plain" => Self::Plain,
            _ => Self::Git,
        }
    }
}

/// One git worktree; the unit of isolation a workspace is scoped to.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Worktree {
    pub project: ProjectName,
    /// Immutable identity, assigned once at creation. See [`WorkspaceId`].
    pub name: String,
    /// The live branch, reconciled against git on each sync tick.
    pub branch: String,
    pub path: PathBuf,
    /// The checked-out commit, absent for a worktree that has never had one.
    pub head: Option<String>,
    /// Pinned workspaces sort first and are never pruned automatically.
    pub pinned: bool,
    /// Archived workspaces stay registered but leave the active project tree.
    #[serde(default)]
    pub archived: bool,
}

impl Worktree {
    /// The id everything else in the system hangs off.
    pub fn workspace_id(&self) -> WorkspaceId {
        WorkspaceId::new(&self.project, &self.name)
    }
}

/// How a worktree stands against its upstream.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchStatus {
    /// Tracked files are modified, staged or untracked files exist.
    pub dirty: bool,
    /// A merge or rebase left unresolved paths.
    pub conflict: bool,
    /// Commits the worktree has that its upstream does not.
    pub ahead: u32,
    /// Commits the upstream has that the worktree does not.
    pub behind: u32,
    /// There is no upstream to compare against, so `ahead` and `behind` mean
    /// nothing and the UI must not render them as zeroes.
    pub untracked_branch: bool,
}

impl BranchStatus {
    /// Whether there is anything worth drawing next to the branch name.
    pub fn is_clean(&self) -> bool {
        !self.dirty && !self.conflict && self.ahead == 0 && self.behind == 0
    }
}

/// Where a workspace's pull request stands on its host.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    /// Open and ready for review.
    Open,
    /// Open, but marked as a draft.
    Draft,
    /// Merged into its base.
    Merged,
    /// Closed without being merged.
    Closed,
}

/// The pull request opened from a workspace's branch.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    /// The host's number for it, `#12`.
    pub number: u32,
    /// Where it is read.
    pub url: String,
    pub state: PullRequestState,
}

/// One agent conversation, scoped to one workspace and one driver.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub workspace: WorkspaceId,
    /// The driver's id: `claude`, `codex`, …
    pub agent: String,
    /// The login it runs on (`docs/accounts.md` §5). Text rather than a
    /// reference: a session outlives the account it ran on, and still says
    /// which one that was.
    pub account: AccountId,
    pub model: Option<String>,
    /// Provider reasoning level reused by every turn in this conversation.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Provider service tier reused by every turn in this conversation.
    #[serde(default)]
    pub service_tier: Option<String>,
    pub state: SessionState,
    /// What this conversation is about.
    ///
    /// Taken from the opening prompt and renameable. Distinct from `summary`,
    /// which is what the agent is doing *now* and changes every turn: a title
    /// is how a conversation is found again a week later.
    pub title: Option<String>,
    /// One line describing what the agent is doing, for the sidebar.
    pub summary: Option<String>,
    /// The vendor's own session id, when it has one. This is what a resume is
    /// built from; without it a session can only be replayed, not continued.
    pub vendor_session_id: Option<String>,
    /// What the agent may touch without asking. Fixed when the session starts
    /// and carried on every follow-up: a transport cannot widen it on a
    /// running process (`docs/roadmap.md` §3.3 N2).
    #[serde(default)]
    pub access_mode: AccessMode,
    /// Where the conversation came from, when a chat platform started it.
    ///
    /// `None` for a session started from the window or the CLI. This is the
    /// thread-to-session map for connectors and what the sidebar draws as an
    /// origin chip (`docs/connectors.md` §7).
    #[serde(default)]
    pub origin: Option<SessionOrigin>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds; moves on every event, and drives the "last activity" sort.
    pub updated_at: i64,
}

/// Where a session came from, when a chat platform started it.
///
/// The three fields together are unique among sessions that are still
/// answering their thread: a reply in the thread finds its session by them.
/// The values are the platform's own ids, opaque to everything but the
/// connector that wrote them.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionOrigin {
    /// Which connector: `slack`.
    pub connector: String,
    /// The platform's conversation id — a Slack channel like `C0123…`.
    pub channel: String,
    /// The platform's thread key — a Slack root message's `ts`.
    pub thread: String,
}

/// What a client sees of one chat connector.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorState {
    /// `slack`.
    pub id: String,
    /// Whether the settings file turns it on. Off means nothing below is
    /// meaningful.
    pub enabled: bool,
    /// Whether the platform connection is up right now.
    pub connected: bool,
    /// Unix seconds when the current connection was made, if it is up.
    pub since: Option<i64>,
    /// The last thing that went wrong, kept until the next success.
    pub last_error: Option<String>,
    /// The channels it listens in, as configured.
    pub bindings: Vec<ConnectorBinding>,
}

/// One channel a connector listens in, as a client sees it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorBinding {
    /// The platform's conversation id.
    pub channel: String,
    /// Where a conversation runs: a workspace id or a project name.
    pub target: String,
    /// The driver id it starts.
    pub agent: String,
    /// `mention` or `all`.
    pub trigger: String,
    /// `shared` or `per_thread`.
    pub worktree: String,
}

impl Session {
    /// Whether the daemon still has a process behind this session.
    pub fn is_live(&self) -> bool {
        matches!(
            self.state,
            SessionState::Starting | SessionState::Running | SessionState::AwaitingInput
        )
    }

    /// Whether the user has to do something before the agent can continue.
    ///
    /// This is the "needs attention" rule the dashboard sorts on: a question or
    /// a failure pulls a workspace to the top, a finished turn does not.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self.state,
            SessionState::AwaitingInput | SessionState::Failed
        )
    }
}

/// Where a session is in its lifecycle.
///
/// The daemon is the only writer. `Idle` means the process is gone but the
/// transcript can be resumed; `AwaitingInput` means the process is alive and
/// blocked on the user.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The driver has been asked to start but has not produced an event yet.
    Starting,
    /// The agent is working.
    Running,
    /// The agent asked a question or proposed a plan and is blocked on a reply.
    AwaitingInput,
    /// The turn ended cleanly and the process exited; resumable.
    Idle,
    /// The agent completed the whole task it was given.
    Finished,
    /// The process exited non-zero, or the driver could not parse its output.
    Failed,
    /// The user cancelled it.
    Cancelled,
}

impl SessionState {
    /// The value stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::AwaitingInput => "awaiting_input",
            Self::Idle => "idle",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a stored value.
    ///
    /// An unrecognised value reads as `Failed` rather than as a live state: a
    /// session the daemon cannot account for must never look like one it is
    /// still driving.
    pub fn parse(value: &str) -> Self {
        match value {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "awaiting_input" => Self::AwaitingInput,
            "idle" => Self::Idle,
            "finished" => Self::Finished,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

/// One follow-up waiting behind the session's active turn.
///
/// The id is stable for the lifetime of the daemon so edits, removal and
/// reordering can never target a neighbouring message after the list changes.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Session-local monotonic identifier.
    pub id: u64,
    /// Prompt text that will be recorded when this message is dispatched.
    pub text: String,
}

/// One recorded position in a session's transcript.
///
/// The transcript is an append-only log of normalized events, not a list of
/// rendered messages: the UI folds deltas into paragraphs, and re-folding is
/// cheaper than losing the tool calls and reasoning that sat between them.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Position within the session, from 1. Also the pagination cursor.
    pub seq: u64,
    /// Unix seconds.
    pub at: i64,
    pub payload: TranscriptPayload,
}

/// Who produced a transcript entry.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum TranscriptPayload {
    /// Something the user sent: the opening prompt, or a follow-up.
    User { text: String },
    /// An answer delivered to an interaction inside a running turn.
    ///
    /// This is distinct from a follow-up because the request id is what lets
    /// a replay close exactly the card that was answered.
    Response { request_id: String, text: String },
    /// Anything the driver emitted, already normalized.
    Agent { event: crate::event::AgentEvent },
}

/// One file in a workspace, as a picker shows it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the worktree root, which is what a mention inserts.
    pub path: String,
    /// The last segment, shown apart from the rest so a list reads as names.
    pub name: String,
    /// How well it matched, for ordering. Zero when nothing was typed.
    pub score: u32,
}

/// What a stretch of work cost.
///
/// Totalled from the usage events the drivers report, which are cumulative per
/// session as the vendor sends them — so a total is the sum of each session's
/// last word, not of every event.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    /// `None` when no vendor involved priced its work; `Some(0.0)` would claim
    /// it was free.
    pub cost_usd: Option<f64>,
    /// How many turns are behind these numbers.
    pub turns: u32,
    /// Whether some of `cost_usd` was priced from the public rate table
    /// rather than reported by the vendor (§3.3 N13).
    #[serde(default)]
    pub estimated: bool,
    /// Sessions whose cost neither the vendor nor the rate table knows, and
    /// which `cost_usd` therefore leaves out.
    #[serde(default)]
    pub unpriced: u32,
}

/// What one day, agent or session cost.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    /// What this row is about: a date, an agent's id, or a session's title.
    pub label: String,
    pub totals: UsageTotals,
}

/// One login of one provider (`docs/accounts.md`).
///
/// Not a credential. An account is a directory the vendor's CLI keeps its own
/// login in; Ginka points the CLI at it and never reads what is inside.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub provider: ProviderKind,
    /// What the chip says.
    pub label: String,
    /// The directory the provider's CLI keeps this login in. A daemon-host
    /// path (`docs/roadmap.md` §4.1). `None` for the provider's default, which
    /// is wherever the CLI keeps it when told nothing.
    pub home: Option<PathBuf>,
    /// The provider's own default, which cannot be removed.
    pub is_default: bool,
    /// Selected for new sessions of this provider. Running sessions keep the
    /// account they started on.
    #[serde(default)]
    pub active: bool,
    /// The *names* of the variables the account's `env` sets. The values never
    /// cross the wire (`docs/accounts.md` §10).
    pub env_keys: Vec<String>,
    /// What the last probe said, when there has been one. `None` when the
    /// provider cannot be asked, which is not the same as signed out.
    pub signed_in: Option<bool>,
    /// The vendor's own sign-in, to run in a terminal with the account's
    /// environment. `None` for a provider whose CLI has no such command.
    pub login: Option<LoginCommand>,
    /// Who the login is, as the vendor's CLI reported it with the probe.
    /// `None` when it was not asked or does not say.
    #[serde(default)]
    pub identity: Option<AccountIdentity>,
}

/// Who a login is: what tells two logins of one provider apart at a glance
/// (MonoCode shows the same in its account picker).
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    /// The address the login is signed in as.
    pub email: Option<String>,
    /// The organisation the login bills to.
    pub organization: Option<String>,
    /// The subscription, in the vendor's own word (`max`, `pro`, `plus`).
    pub plan: Option<String>,
}

impl AccountIdentity {
    /// One line for a picker row or a tooltip: `email · organisation · plan`,
    /// leaving out what the vendor did not say and an organisation that only
    /// repeats the email (a personal login's default organisation is named
    /// after it).
    pub fn summary(&self) -> Option<String> {
        let organization = self.organization.as_ref().filter(|organization| {
            self.email
                .as_ref()
                .is_none_or(|email| !organization.starts_with(email.as_str()))
        });
        let parts: Vec<&str> = [self.email.as_ref(), organization, self.plan.as_ref()]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .filter(|part| !part.trim().is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// A command a client runs on the user's behalf, in a terminal.
///
/// Carries the environment that names the account's directory and nothing
/// else: a login does not need the account's own variables, and one of those
/// may be a key.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A subscription's rate-limit window (`docs/roadmap.md` §3.3 N12).
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanWindow {
    /// What the vendor calls it, or what its length says: `5h`, `week`.
    pub label: String,
    pub used_percent: f64,
    /// Unix seconds, when the provider tells us.
    pub resets_at: Option<i64>,
}

impl PlanWindow {
    /// How much of the window is left, clamped to a percentage.
    pub fn remaining_percent(&self) -> f64 {
        (100.0 - self.used_percent.clamp(0.0, 100.0)).clamp(0.0, 100.0)
    }

    /// Whether the wall has been hit.
    pub fn is_exhausted(&self) -> bool {
        self.used_percent >= 100.0
    }

    /// "resets in 2h 15m", or nothing at all when the provider did not say.
    /// A guess here would be worse than silence: the user plans around it.
    pub fn reset_label(&self, now: i64) -> String {
        let Some(resets_at) = self.resets_at else {
            return String::new();
        };
        let remaining = resets_at - now;
        if remaining <= 0 {
            return "resets now".to_string();
        }
        let hours = remaining / 3_600;
        let minutes = (remaining % 3_600) / 60;
        match (hours, minutes) {
            (0, 0) => "resets in under a minute".to_string(),
            (0, minutes) => format!("resets in {minutes}m"),
            (hours, 0) => format!("resets in {hours}h"),
            (hours, minutes) => format!("resets in {hours}h {minutes}m"),
        }
    }
}

/// Every rate-limit window an account has, as last reported.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanUsage {
    /// The vendor's name for the plan, when it says: `pro`, `max`.
    pub plan: Option<String>,
    pub windows: Vec<PlanWindow>,
}

impl PlanUsage {
    /// The window closest to its ceiling — the one worth the space in the UI.
    pub fn tightest(&self) -> Option<&PlanWindow> {
        self.windows.iter().max_by(|left, right| {
            left.used_percent
                .partial_cmp(&right.used_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

/// Where a reading of an account's windows came from.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    /// A turn carried it: the vendor reports its windows as it works.
    Reported,
    /// Asked for, with no turn run.
    Fetched,
}

/// The latest reading of an account's rate-limit windows.
///
/// A gauge, not an event: what matters is the newest reading and when it was
/// taken. A gauge without an age is a claim (`docs/accounts.md` §6).
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanSnapshot {
    pub account: AccountId,
    pub usage: PlanUsage,
    /// Unix seconds.
    pub observed_at: i64,
    pub source: PlanSource,
}

/// Which side of a diff a line number belongs to.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    /// The file as it was.
    Old,
    /// The file as the agent left it, which is what a comment usually means.
    New,
}

/// A note left on a diff, waiting to be sent back to the agent.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: String,
    pub workspace: WorkspaceId,
    /// Relative to the worktree root.
    pub path: String,
    /// `None` is a comment on the file as a whole, which people write.
    pub line: Option<u32>,
    /// The last line of a comment that covers several, after `line`;
    /// `None` for a comment on one line or the whole file.
    #[serde(default)]
    pub end_line: Option<u32>,
    pub side: DiffSide,
    pub text: String,
    /// Unix seconds.
    pub created_at: i64,
}

/// Where a slash command came from.
///
/// The scope is shown beside the name because two commands can share one: a
/// project's `/review` and the user's own `/review` are different commands, and
/// which one runs is decided by this.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    /// Handled by Ginka before a prompt reaches a provider.
    BuiltIn,
    /// Defined in the workspace, so it travels with the code.
    Project,
    /// Defined in the user's own home, so it travels with them.
    User,
}

/// One local branch, as a picker lists it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    /// Checked out in the workspace that was asked.
    pub current: bool,
    /// The worktree that has it checked out, when one does — git refuses to
    /// check a branch out twice, so a picker greys these. A daemon-host path.
    pub checked_out_at: Option<PathBuf>,
}

/// Where a skill was installed from the user's point of view.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    /// Installed for the user, wherever an ecosystem keeps them.
    User,
    /// Checked into the project it belongs to.
    Project,
}

/// One copy of a skill: a directory holding a `SKILL.md`.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInstall {
    /// The root it was found under, as the library names it: `claude`,
    /// `codex`, `agents`, or a project's name.
    pub root_label: String,
    pub scope: SkillScope,
    /// The skill's own directory, not the file inside it. A daemon-host path
    /// (`docs/roadmap.md` §4.1).
    pub directory: PathBuf,
    pub enabled: bool,
}

/// A skill the agents can load, grouped across every place it was installed
/// (`docs/roadmap.md` §3.3 N11).
///
/// Installers and dotfiles drop the same skill into several ecosystems'
/// roots; one entry carries every copy, and a toggle applies to all of them.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// From the file's front matter, or the directory's name.
    pub name: String,
    pub description: Option<String>,
    /// True only when every copy is enabled: a skill half-hidden is a skill
    /// the user cannot rely on, and the toggle should say so.
    pub enabled: bool,
    pub installs: Vec<SkillInstall>,
}

/// A command the composer offers after `/`.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    /// Without the slash: `review`, `test`, `frontend/fix`.
    pub name: String,
    /// One line, from the file's frontmatter or its first heading.
    pub description: String,
    pub scope: CommandScope,
    /// What the command expects after its name, when it says.
    pub argument_hint: Option<String>,
}

/// Where a search found something.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMatch {
    pub session: SessionId,
    pub workspace: WorkspaceId,
    pub title: Option<String>,
    /// The transcript position, so the client can page straight to it.
    pub seq: u64,
    /// Unix seconds.
    pub at: i64,
    /// The line the query was found in, cut to something a list can show.
    pub excerpt: String,
}

/// A conversation an agent's own CLI keeps, run outside Ginka in the same
/// directory — what the Resume-from-CLI list offers to bring in.
///
/// Read from the vendor's store on the daemon host (`~/.claude/projects`,
/// `~/.codex/sessions`); adopting it resumes that same vendor thread.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliSession {
    /// The driver that can resume it: `claude` or `codex`.
    pub agent: String,
    /// The vendor's own session id, which is what a resume is built from.
    pub vendor_session_id: String,
    /// The title the CLI gave it, or else its opening prompt, cut short.
    pub title: String,
    /// How many prompts the user sent in it.
    pub prompts: u32,
    /// Unix seconds of the last thing written to it.
    pub updated_at: i64,
}

/// A workspace state snapshotted at a turn boundary, so a transcript position
/// maps to a working tree the user can go back to.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub session: SessionId,
    pub workspace: WorkspaceId,
    /// The turn this snapshot was taken at the end of, from 1.
    pub turn: u32,
    /// The commit holding the snapshot. It is not on any branch: restoring
    /// reads the tree out of it, so the user's own history is never rewritten.
    pub commit: String,
    /// What the transcript said at this point, for the rewind menu.
    pub label: String,
    /// Whether a saved start snapshot makes this checkpoint a completed turn
    /// with an exact diff. Initial and restore-safety checkpoints lack one.
    pub has_turn_start: bool,
    /// Unix seconds.
    pub created_at: i64,
}

/// What is known about one agent CLI on this machine.
///
/// Answered by probing: an agent that is not installed, or installed but not
/// signed in, is something the user has to be told *before* they send a
/// prompt. Finding out from a failed session is finding out too late.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    /// The driver id: `claude`, `codex`, …
    pub id: String,
    /// What the agent picker shows.
    pub display_name: String,
    /// The binary that would be run, after any override in `settings.json`.
    pub program: String,
    /// Whether the binary could be run at all.
    pub installed: bool,
    /// What it reported as its version.
    pub version: Option<String>,
    /// `None` when this driver has no way to ask; the agent may still work.
    pub authenticated: Option<bool>,
    /// What the CLI said about itself, when it said anything useful.
    pub detail: Option<String>,
    /// The models this driver offers, including the options each accepts.
    ///
    /// A live CLI catalogue wins; drivers supply a static fallback so the
    /// picker remains useful offline (`docs/roadmap.md` §3.3 N3).
    pub models: Vec<crate::provider::ProviderModel>,
}

impl AgentStatus {
    /// Whether a session started with this agent has a chance of working.
    ///
    /// An agent that cannot say whether it is signed in counts as ready: the
    /// alternative is refusing to start an agent that would have worked.
    pub fn is_ready(&self) -> bool {
        self.installed && self.authenticated != Some(false)
    }
}

/// What a set of changes was asked for against.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "against", rename_all = "snake_case")]
pub enum ChangeSource {
    /// Everything not committed, staged or not, including files git has never
    /// seen — an agent's new file is the change most worth reading.
    Uncommitted,
    /// Changes in the worktree that are not yet in the index.
    Unstaged,
    /// What is staged for the next commit.
    Staged,
    /// What has happened since a checkpoint, including later worktree edits.
    SinceCheckpoint { checkpoint: CheckpointId },
    /// Only the changes made during the completed turn ending at this
    /// checkpoint. Later worktree edits are excluded.
    Turn { checkpoint: CheckpointId },
    /// What one commit did, against its first parent: what a row of the
    /// history opens. A merge reads as what it brought into the branch.
    ///
    /// `commit` is a hexadecimal object name, never a ref or a range: it is
    /// handed to git as an argument, and anything that could be read as an
    /// option or a revision expression is refused before it gets there.
    Commit { commit: String },
    /// Everything the branch did since it left `base` — its commits and
    /// whatever is not committed yet — measured from their merge base, so
    /// later commits on the base are not counted (Orca's base-branch diff).
    /// `None` is the project's default branch. `base` must name a branch or
    /// remote-tracking branch; anything else is refused before git runs.
    Branch {
        #[serde(default)]
        base: Option<String>,
    },
}

/// How one file changed.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// One line of a diff.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// Unchanged, shown for context.
    Context,
    Added,
    Removed,
}

/// One line of a hunk, with the numbers it has on each side.
///
/// Both numbers are kept because a review comment is anchored to a line on one
/// side or the other, and a hunk header alone cannot say which.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// The line's number before the change, absent for an added line.
    pub old_line: Option<u32>,
    /// Its number after, absent for a removed line.
    pub new_line: Option<u32>,
    /// The parts of `text` that actually differ from the line it replaced.
    ///
    /// Empty when there is nothing worth pointing at — a line with no partner,
    /// or one rewritten so completely that marking it up would be marking all
    /// of it. Byte ranges into `text`, in order and not overlapping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Span>,
    /// For an added line in a diff of the worktree: whether an agent's
    /// turn wrote it (`true`) or a person did (`false`). Absent where it
    /// was not worked out — other sources, and context or removed lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_agent: Option<bool>,
}

/// A range of bytes in a line.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// A run of changed lines with the context around it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// The `@@ … @@` line, including whatever git put after it.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// One file's changes.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Relative to the worktree root.
    pub path: String,
    /// Where it came from, for a rename.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
    /// A binary file has counts but no hunks: there is nothing to read.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

/// What merging one branch into another did.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeOutcome {
    /// The branch that moved.
    pub into: String,
    /// Where it is now.
    pub commit: String,
    /// Whether it moved without a merge commit.
    pub fast_forward: bool,
}

/// One commit in a workspace's recent history.
///
/// Parent ids are retained so a client can draw branch and merge topology
/// without interpreting presentation text from `git log`.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommit {
    /// Full object id of the commit.
    pub id: String,
    /// Full object ids of its parents, empty for a root commit.
    pub parents: Vec<String>,
    /// Author name recorded in the commit.
    pub author: String,
    /// Author time as Unix seconds.
    pub authored_at: i64,
    /// First line of the commit message.
    pub summary: String,
    /// Whether a remote-tracking branch already has this commit, as of the
    /// last fetch. An unpublished HEAD is one that can still be amended.
    #[serde(default)]
    pub published: bool,
}

impl FileChange {
    /// The path to show: a rename is best read as `old → new`.
    pub fn label(&self) -> String {
        match &self.old_path {
            Some(old) if old != &self.path => format!("{old} → {}", self.path),
            _ => self.path.clone(),
        }
    }
}

/// Everything that changed, and what it was measured against.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    pub source: ChangeSource,
    pub files: Vec<FileChange>,
}

impl Changes {
    /// Lines added and removed across every file.
    pub fn totals(&self) -> (u32, u32) {
        self.files.iter().fold((0, 0), |(added, removed), file| {
            (added + file.added, removed + file.removed)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// One line of a file that matched a search.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentMatch {
    /// Relative to the worktree root.
    pub path: String,
    /// 1-based, as an editor counts them.
    pub line: u32,
    /// The line itself, as it is in the file.
    pub text: String,
}

/// One content-search hit together with the worktree that contains it.
///
/// Project-wide search keeps paths relative to their own worktree, so the
/// workspace id is the missing part a client needs to display or open one.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContentMatch {
    /// Immutable workspace containing the matched file.
    pub workspace: WorkspaceId,
    /// Path relative to that workspace's worktree root.
    pub path: String,
    /// 1-based line number, as an editor counts it.
    pub line: u32,
    /// The bounded source line containing the literal query.
    pub text: String,
}

/// One fuzzy path-search hit together with the worktree that contains it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileMatch {
    /// Immutable workspace containing the file.
    pub workspace: WorkspaceId,
    /// Path relative to that workspace's worktree root.
    pub path: String,
}

/// A file, as the panel that shows it needs it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContent {
    /// Relative to the worktree root.
    pub path: String,
    /// UTF-8 preview, empty for binary content and bounded when truncated.
    pub text: String,
    /// Opaque digest of the complete bytes read from disk.
    ///
    /// A save sends this back so an edit made elsewhere is never overwritten
    /// by a stale editor buffer.
    pub revision: String,
    /// A binary file has no text worth showing; saying so beats showing none.
    pub binary: bool,
    /// Whether there is more of it than was sent.
    pub truncated: bool,
    /// A recognized, bounded image payload suitable for a local preview.
    ///
    /// Unsupported and oversized binary files omit it. The daemon determines
    /// the media type from the bytes rather than trusting the file extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<FileImage>,
}

/// Image bytes carried with a file read for a controlled local preview.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileImage {
    /// An allow-listed media type inferred from the file signature.
    pub media_type: String,
    /// The complete image bytes encoded for the JSON wire protocol.
    pub data_base64: String,
}

/// One shell the daemon is running, as a tab strip sees it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub workspace: WorkspaceId,
    /// What it is called in the strip: `shell 1`, `shell 2`, per workspace.
    pub title: String,
}

/// A workspace with everything the dashboard draws for it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub worktree: Worktree,
    pub status: BranchStatus,
    /// The most recently updated session, if any.
    pub session: Option<Session>,
    /// Unix seconds of the worktree's HEAD commit, for the "last activity" line.
    pub last_commit_at: Option<i64>,
    /// Whether zvec-grep's index directory exists in this worktree.
    #[serde(default)]
    pub indexed: bool,
    /// How many follow-ups wait in the latest session's queue.
    #[serde(default)]
    pub queued: u32,
    /// The pull request opened from the worktree's branch, when the daemon
    /// has seen one. `None` also for a project with no GitHub remote or a
    /// machine without `gh`: the listing never waits on the network.
    #[serde(default)]
    pub pull_request: Option<PullRequest>,
    /// The line of status last written on the workspace — by its agent over
    /// MCP, or by a person — and when. Orca's worktree comment.
    #[serde(default)]
    pub status_note: Option<StatusNote>,
}

/// An MCP server an agent CLI is configured with, outside Ginka.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    /// Whose configuration names it: `claude`, `codex`.
    pub provider: String,
    /// Where it is configured.
    pub scope: McpScope,
    /// The command it runs, or the address it is reached at — never its
    /// arguments or environment, and an address with its user information,
    /// query and fragment replaced by `…`, since any of them can carry a key.
    pub target: Option<String>,
}

/// Whether a newer release of an agent CLI is out.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentUpdate {
    /// The driver id: `claude`, `codex`, …
    pub agent: String,
    /// What the probe read from the installed CLI.
    pub installed: Option<String>,
    /// The latest published version, when the registry could be read.
    pub latest: Option<String>,
    pub update_available: bool,
    /// The command that updates it, for the reader to run.
    pub command: String,
}

/// An MCP server to add to an agent CLI's configuration.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    /// Whose configuration it goes into: `claude`, `codex`.
    pub provider: String,
    /// Where it is written; Codex has user scope only.
    pub scope: McpScope,
    /// How it is reached.
    pub target: McpTarget,
}

/// How an MCP server is reached.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTarget {
    /// A program the agent starts and speaks to over stdio.
    Command {
        /// The program, found on the agent's PATH.
        program: String,
        /// Its arguments, as given.
        args: Vec<String>,
    },
    /// A streamable HTTP server.
    Url {
        /// An `http://` or `https://` address.
        url: String,
    },
}

/// Where an MCP server is configured.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    /// For every project, in the user's own configuration.
    User,
    /// Checked into the repository (`.mcp.json`).
    Project,
    /// For this project only, in the user's configuration.
    Local,
}

/// One check on a pull request, as `gh pr checks` reports it.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    /// The workflow it belongs to, when it is an Actions job.
    pub workflow: Option<String>,
    /// Where it stands.
    pub state: CheckState,
    /// Where its log is.
    pub link: Option<String>,
}

/// Where a check stands.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// It ran and succeeded.
    Passed,
    /// It ran and failed: what "Fix failing checks" hands over.
    Failed,
    /// Queued or running — or a state this build does not know.
    Pending,
    /// Its conditions did not apply, so it did not run.
    Skipped,
    /// It was stopped before it finished.
    Cancelled,
}

/// A short, free-text line saying where a workspace's work stands.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusNote {
    /// One line, at most [`StatusNote::MAX_CHARS`] characters.
    pub text: String,
    /// Unix seconds it was written.
    pub set_at: i64,
}

impl StatusNote {
    /// Longest note kept. A sidebar row shows one line; a paragraph belongs
    /// in the transcript or in Notes.
    pub const MAX_CHARS: usize = 160;

    /// What `text` is stored as: its first non-blank line, trimmed and
    /// bounded, or `None` when there is nothing left — which clears the note.
    pub fn normalize(text: &str) -> Option<String> {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        if line.chars().count() <= Self::MAX_CHARS {
            return Some(line.to_string());
        }
        let cut: String = line.chars().take(Self::MAX_CHARS - 1).collect();
        Some(format!("{}…", cut.trim_end()))
    }
}

impl WorkspaceSummary {
    /// The id this summary is about.
    pub fn id(&self) -> WorkspaceId {
        self.worktree.workspace_id()
    }

    /// Whether this workspace should sort to the top of the sidebar.
    pub fn needs_attention(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.needs_attention())
            || self.status.conflict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree() -> Worktree {
        Worktree {
            project: ProjectName("comet".into()),
            name: "bright-harbor".into(),
            branch: "bright-harbor".into(),
            path: PathBuf::from("/tmp/wt"),
            head: None,
            pinned: false,
            archived: false,
        }
    }

    fn session_in(state: SessionState) -> Session {
        Session {
            id: SessionId("s-1".into()),
            workspace: WorkspaceId("comet/bright-harbor".into()),
            agent: "claude".into(),
            account: AccountId("claude".into()),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            state,
            title: None,
            summary: None,
            vendor_session_id: None,
            access_mode: AccessMode::default(),
            origin: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn workspace_id_survives_a_branch_switch() {
        let mut worktree = worktree();
        let before = worktree.workspace_id();
        // An agent checks out a different branch inside the worktree.
        worktree.branch = "some/other-branch".into();
        assert_eq!(worktree.workspace_id(), before);
    }

    #[test]
    fn session_states_round_trip_through_storage() {
        for state in [
            SessionState::Starting,
            SessionState::Running,
            SessionState::AwaitingInput,
            SessionState::Idle,
            SessionState::Finished,
            SessionState::Failed,
            SessionState::Cancelled,
        ] {
            assert_eq!(SessionState::parse(state.as_str()), state);
        }
    }

    #[test]
    fn an_unknown_stored_state_never_reads_as_live() {
        let state = SessionState::parse("teleporting");
        assert_eq!(state, SessionState::Failed);
        assert!(!session_in(state).is_live());
    }

    #[test]
    fn attention_is_a_question_or_a_failure_not_a_finished_turn() {
        assert!(session_in(SessionState::AwaitingInput).needs_attention());
        assert!(session_in(SessionState::Failed).needs_attention());
        assert!(!session_in(SessionState::Finished).needs_attention());
        assert!(!session_in(SessionState::Running).needs_attention());
    }

    #[test]
    fn a_conflicted_worktree_needs_attention_even_with_no_session() {
        let summary = WorkspaceSummary {
            worktree: worktree(),
            status: BranchStatus {
                conflict: true,
                dirty: true,
                ..BranchStatus::default()
            },
            session: None,
            last_commit_at: None,
            indexed: false,
            queued: 0,
            pull_request: None,
            status_note: None,
        };
        assert!(summary.needs_attention());
        assert_eq!(summary.id().0, "comet/bright-harbor");
    }

    #[test]
    fn an_identity_reads_as_one_line_without_repeating_the_email() {
        let personal = AccountIdentity {
            email: Some("me@example.com".into()),
            organization: Some("me@example.com's Organization".into()),
            plan: Some("max".into()),
        };
        assert_eq!(personal.summary().as_deref(), Some("me@example.com · max"));
        let work = AccountIdentity {
            organization: Some("Acme".into()),
            ..personal
        };
        assert_eq!(
            work.summary().as_deref(),
            Some("me@example.com · Acme · max")
        );
        assert_eq!(AccountIdentity::default().summary(), None);
    }

    #[test]
    fn a_status_note_is_one_bounded_line_and_blank_clears_it() {
        assert_eq!(StatusNote::normalize("  \n  "), None);
        assert_eq!(
            StatusNote::normalize("\n  waiting on CI  \nmore detail"),
            Some("waiting on CI".to_string())
        );
        let long = "x".repeat(400);
        let kept = StatusNote::normalize(&long).unwrap();
        assert_eq!(kept.chars().count(), StatusNote::MAX_CHARS);
        assert!(kept.ends_with('…'));
    }

    #[test]
    fn an_older_workspace_summary_defaults_to_not_indexed() {
        let json = serde_json::to_value(WorkspaceSummary {
            worktree: worktree(),
            status: BranchStatus::default(),
            session: None,
            last_commit_at: None,
            indexed: false,
            queued: 0,
            pull_request: None,
            status_note: None,
        })
        .unwrap();
        let mut object = json.as_object().unwrap().clone();
        object.remove("indexed");
        object.remove("pull_request");
        let summary: WorkspaceSummary = serde_json::from_value(object.into()).unwrap();
        assert!(!summary.indexed);
        assert_eq!(summary.pull_request, None);
    }
}

/// A markdown note, kept by the daemon.
///
/// MonoCode's notebook (`docs/monocode-parity.md`): somewhere to keep what a
/// session taught the reader that is not code. Belongs to a project, or to
/// none — a note written before a project exists is still worth keeping.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub project: Option<ProjectName>,
    pub title: String,
    /// Markdown.
    pub body: String,
    /// Lowercase, distinct labels used to organize and find the note.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds; notes are listed most recently touched first.
    pub updated_at: i64,
}

/// What a quick command does when it is run.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuickCommandKind {
    /// A shell command, run in a terminal in the workspace.
    Shell,
    /// A prompt, sent to the workspace's agent.
    Prompt,
}

impl QuickCommandKind {
    /// The value stored in SQLite and printed by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Prompt => "prompt",
        }
    }

    /// Read a stored or typed value.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "shell" => Some(Self::Shell),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }
}

/// A page from a workspace's browser history.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitedPage {
    pub url: String,
    pub title: Option<String>,
    pub visits: u32,
    /// Unix seconds.
    pub last_visited_at: i64,
}

/// One of Ginka's own skills, installed into one place.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundledSkillInstall {
    pub name: String,
    /// The `SKILL.md` it concerns, on the daemon's host.
    pub path: String,
    /// `written`, `unchanged`, or `kept` — a different file, or one the
    /// reader turned off, left alone.
    pub outcome: String,
}

/// How a scheduled job runs.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronVia {
    /// The body is a prompt, started as a new conversation with an agent.
    Chat,
    /// The body is a shell command, run in a terminal that closes with it.
    Terminal,
}

impl CronVia {
    /// The value stored in SQLite and printed by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Terminal => "terminal",
        }
    }

    /// Read a stored or typed value.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "chat" => Some(Self::Chat),
            "terminal" => Some(Self::Terminal),
            _ => None,
        }
    }
}

/// What one firing of a scheduled job came to.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronOutcome {
    /// It is running.
    Started,
    /// Its terminal closed.
    Finished,
    /// The previous run was still going, so this one did not start.
    Skipped,
    /// It could not be started; `detail` says why.
    Failed,
}

impl CronOutcome {
    /// The value stored in SQLite and printed by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Finished => "finished",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    /// Read a stored value.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "started" => Some(Self::Started),
            "finished" => Some(Self::Finished),
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A prompt or command run on a schedule, in one workspace — or in the
/// project's own checkout when `workspace` is absent.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronJob {
    pub id: i64,
    pub project: ProjectName,
    pub workspace: Option<WorkspaceId>,
    /// The conversation a chat job continues, instead of starting a new
    /// one each time — a reminder.
    #[serde(default)]
    pub session: Option<SessionId>,
    pub name: String,
    /// Five cron fields or `@daily` and the like on the daemon host's clock,
    /// or `@once <RFC3339 timestamp with time zone>` for one firing.
    pub schedule: String,
    pub via: CronVia,
    /// The driver a chat job starts. Absent for a terminal job.
    pub agent: Option<String>,
    /// The prompt, or the command line.
    pub body: String,
    /// A shell command run in the job's checkout before a scheduled firing;
    /// a non-zero exit, or no answer within a minute, skips that firing.
    #[serde(default)]
    pub precheck: Option<String>,
    pub enabled: bool,
    /// Unix seconds of the next firing; absent while disabled.
    pub next_run_at: Option<i64>,
    /// The most recent firing, if it has fired.
    pub last_run: Option<CronRun>,
}

/// One firing of a scheduled job.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronRun {
    pub id: i64,
    pub job: i64,
    /// Unix seconds.
    pub started_at: i64,
    /// Unix seconds; set when a terminal job's terminal closed.
    pub finished_at: Option<i64>,
    pub outcome: CronOutcome,
    /// Why it failed, or the conversation or terminal it started.
    pub detail: Option<String>,
}

/// A saved command or prompt — Orca's Quick Commands — for one project, or
/// for every project when `project` is absent.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickCommand {
    pub id: String,
    pub project: Option<ProjectName>,
    /// What the menu and the palette call it.
    pub name: String,
    pub kind: QuickCommandKind,
    /// The shell command line, or the prompt text.
    pub body: String,
}

/// Where a ticket is in its short life.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketState {
    /// Raised and waiting for the reader to start or dismiss it.
    Open,
    /// A session was started from it; `Ticket::session` says which.
    Started,
    /// The reader, or the agent that raised it, decided against it.
    Dismissed,
}

impl TicketState {
    /// The value stored in SQLite and printed by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Started => "started",
            Self::Dismissed => "dismissed",
        }
    }

    /// Read a stored value.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "open" => Some(Self::Open),
            "started" => Some(Self::Started),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }
}

/// A piece of work an agent noticed and handed to the reader rather than
/// doing in its own session: dead code, a stale doc, a missing test.
///
/// The reader sees it as a card and starts it with one click, which runs
/// `prompt` in a new session — so the work the agent was asked to do stays
/// the size it was asked to be, and the aside is not lost either.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    /// Where it was raised, and where it runs unless a branch is asked for.
    pub workspace: WorkspaceId,
    /// The session that raised it, when an agent did.
    pub from_session: Option<SessionId>,
    /// A short imperative, the card's heading.
    pub title: String,
    /// One or two sentences for the card: why now, and what it will do.
    pub summary: String,
    /// What the new session is told. Self-contained: the session that reads
    /// it has none of the conversation that raised it.
    pub prompt: String,
    pub state: TicketState,
    /// The session started from it.
    pub session: Option<SessionId>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}
