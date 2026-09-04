//! The domain objects as they appear on the wire.
//!
//! These live in the protocol crate rather than in `ginka-core` because the
//! CLI, the app and any future non-Rust client need to name them without
//! linking the daemon's domain logic. `ginka-core` persists them; nothing here
//! knows what a database or a git repository is.

use crate::ids::{CheckpointId, ProjectName, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

/// One agent conversation, scoped to one workspace and one driver.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub workspace: WorkspaceId,
    /// The driver's id: `claude`, `codex`, …
    pub agent: String,
    pub model: Option<String>,
    pub state: SessionState,
    /// One line describing what the agent is doing, for the sidebar.
    pub summary: Option<String>,
    /// The vendor's own session id, when it has one. This is what a resume is
    /// built from; without it a session can only be replayed, not continued.
    pub vendor_session_id: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds; moves on every event, and drives the "last activity" sort.
    pub updated_at: i64,
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
    /// Anything the driver emitted, already normalized.
    Agent { event: crate::event::AgentEvent },
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
    /// The models this driver offers, empty when the vendor decides.
    pub models: Vec<String>,
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
    /// What is staged for the next commit.
    Staged,
    /// What has happened since a checkpoint: the answer to "what did this turn
    /// actually do".
    SinceCheckpoint { checkpoint: CheckpointId },
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
        }
    }

    fn session_in(state: SessionState) -> Session {
        Session {
            id: SessionId("s-1".into()),
            workspace: WorkspaceId("comet/bright-harbor".into()),
            agent: "claude".into(),
            model: None,
            state,
            summary: None,
            vendor_session_id: None,
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
        };
        assert!(summary.needs_attention());
        assert_eq!(summary.id().0, "comet/bright-harbor");
    }
}
