//! The request/response surface of the daemon.
//!
//! This enum *is* the app's capability list. Anything the UI can do appears
//! here, which is what makes the same operation reachable from `ginka` on the
//! command line and from the MCP server (`AGENTS.md` rule 3). Adding a private
//! path from a view into `ginka-core` is how that guarantee gets lost.

use crate::ids::{CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
use crate::model::{
    AgentStatus, ChangeSource, Changes, Checkpoint, DiffSide, FileEntry, Project, ReviewComment,
    Session, SessionMatch, SlashCommand, TranscriptEntry, UsageRow, WorkspaceSummary,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Something a client asks the daemon to do.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Liveness, and the cheapest way to confirm the token was accepted.
    Ping,

    /// Every registered project, in sidebar order.
    ListProjects,
    /// Register a repository or folder, adopting the worktrees it already has.
    AddProject { path: PathBuf },
    /// Forget a project. The worktrees on disk are left alone: the daemon
    /// registered them, it did not create the user's code.
    RemoveProject { project: ProjectName },

    /// Every workspace, reconciled against git first. `project` limits it.
    ListWorkspaces { project: Option<ProjectName> },
    /// Create a worktree on `branch`, cutting it from `base` when the branch
    /// does not exist yet.
    CreateWorkspace {
        project: ProjectName,
        branch: String,
        base: Option<String>,
    },
    /// Make somewhere to work with no project at all.
    ///
    /// The daemon creates a dated directory under its own state, registers it
    /// as a plain project and answers with its workspace. This is the "just
    /// start an agent" flow: a question that needs a scratch directory should
    /// not need a repository first.
    CreateScratchWorkspace { name: Option<String> },
    /// Remove a workspace's worktree. `force` is required when it is dirty.
    RemoveWorkspace { workspace: WorkspaceId, force: bool },
    /// Pin or unpin a workspace.
    PinWorkspace {
        workspace: WorkspaceId,
        pinned: bool,
    },

    /// Which agents this machine has, and whether they are usable.
    ///
    /// Probing runs each vendor's CLI, so this is a request rather than
    /// something a client can work out for itself.
    ListAgents,
    /// Sessions, newest first. `workspace` limits it.
    ListSessions { workspace: Option<WorkspaceId> },
    /// Start an agent in a workspace and send it `prompt`.
    StartSession {
        workspace: WorkspaceId,
        /// A driver id: `claude`, `codex`, …
        agent: String,
        prompt: String,
        model: Option<String>,
    },
    /// Send a follow-up. Queued when the agent is mid-turn, which is why this
    /// answers `Ack` rather than waiting for the reply.
    SendMessage { session: SessionId, text: String },
    /// Answer an [`AskUser`](crate::event::AgentEvent::AskUser) or approve a
    /// [`PlanProposal`](crate::event::AgentEvent::PlanProposal).
    RespondToAgent {
        session: SessionId,
        /// The `id` from the event being answered.
        request_id: String,
        response: String,
    },
    /// Rename a conversation. An empty title clears it back to none.
    RenameSession { session: SessionId, title: String },
    /// Forget a session and its transcript and checkpoints.
    RemoveSession { session: SessionId },
    /// Take a copy of a conversation as it was, and carry on from there.
    ///
    /// `after` is the transcript position to fork at; `None` forks the whole
    /// thing. The fork inherits the vendor's session id, so continuing it
    /// continues the same conversation with the agent.
    ForkSession {
        session: SessionId,
        after: Option<u64>,
    },
    /// Find transcript entries containing `query`.
    SearchSessions {
        workspace: Option<WorkspaceId>,
        query: String,
        limit: Option<u32>,
    },

    /// Kill the agent's process tree.
    CancelSession { session: SessionId },
    /// Read a transcript. `after` pages forward from a sequence number.
    SessionTranscript {
        session: SessionId,
        after: Option<u64>,
        limit: Option<u32>,
    },

    /// What has changed in a workspace, against `source`.
    ///
    /// The review half of the loop: an agent that edited twelve files is only
    /// useful if the twelve are readable without leaving the window.
    WorkspaceChanges {
        workspace: WorkspaceId,
        source: ChangeSource,
    },

    /// Commit a workspace's work.
    ///
    /// `all` stages everything first, including files git has never seen,
    /// which is what a review that just listed those files leads the user to
    /// expect.
    Commit {
        workspace: WorkspaceId,
        message: String,
        all: bool,
    },
    /// Put one file into the next commit, or take it back out.
    ///
    /// Per file, because "these three are right and that one is not" is the
    /// ordinary outcome of reading an agent's work, and committing all of it
    /// is not the only thing a reader may want to do about it.
    StageFile {
        workspace: WorkspaceId,
        path: String,
        /// `true` stages it, `false` takes it back out.
        staged: bool,
    },
    /// Throw away one file's uncommitted work.
    ///
    /// Destructive and not undoable through git: a file the agent invented is
    /// deleted. The caller confirms; the daemon does as it is told.
    RevertFile {
        workspace: WorkspaceId,
        path: String,
    },
    /// Push a workspace's branch, setting an upstream if it has none.
    Push { workspace: WorkspaceId },

    /// The files in a workspace, best matches for `query` first.
    ///
    /// What `@` in the composer reaches for, and what a quick-open will.
    WorkspaceFiles {
        workspace: WorkspaceId,
        query: Option<String>,
        limit: Option<u32>,
    },
    /// The commands a workspace offers after `/`.
    SlashCommands {
        workspace: WorkspaceId,
        query: Option<String>,
    },
    /// What the user was in the middle of typing in a workspace.
    ComposerDraft { workspace: WorkspaceId },
    /// Keep what they are typing. An empty draft forgets it.
    SaveComposerDraft {
        workspace: WorkspaceId,
        text: String,
    },

    /// What the work has cost, by day and by agent.
    Usage { days: Option<u32> },

    /// Leave a comment on a line of the diff.
    ///
    /// Orca's loop: marking the three places that are wrong is a better way to
    /// tell an agent what to fix than re-prompting it from scratch.
    AddReviewComment {
        workspace: WorkspaceId,
        path: String,
        line: Option<u32>,
        side: DiffSide,
        text: String,
    },
    /// Every comment waiting in a workspace, in reading order.
    ListReviewComments { workspace: WorkspaceId },
    /// Take one comment back.
    RemoveReviewComment { comment: String },
    /// Send the batch to the agent as one message, and clear it.
    ///
    /// One message rather than one per comment: an agent given them together
    /// can see that three of them are the same mistake, and a turn per comment
    /// is three times the context and three times the cost.
    SendReviewComments {
        workspace: WorkspaceId,
        /// The conversation to send them to. Its agent gets the batch as a
        /// follow-up, queued if it is still working.
        session: SessionId,
    },

    /// Every checkpoint taken in a workspace, newest first.
    ListCheckpoints { workspace: WorkspaceId },
    /// Put the worktree back to a checkpoint's state.
    RestoreCheckpoint { checkpoint: CheckpointId },

    /// Start a shell in a workspace.
    ///
    /// The daemon owns the pty, so the shell outlives the window that opened
    /// it — which is the whole point of the process split.
    OpenTerminal {
        workspace: WorkspaceId,
        rows: u16,
        cols: u16,
    },
    /// Send keystrokes to a terminal.
    WriteTerminal { terminal: TerminalId, data: String },
    /// Tell a terminal how big its window is now.
    ResizeTerminal {
        terminal: TerminalId,
        rows: u16,
        cols: u16,
    },
    /// Close a terminal and stop its shell.
    CloseTerminal { terminal: TerminalId },

    /// Ask the daemon to exit once it has flushed its state.
    Shutdown,
}

/// What the daemon answers with.
///
/// One variant per shape rather than per request: several requests legitimately
/// answer `Ack`, and a client that has to match on the request it sent to
/// understand the reply is a client that cannot be written generically.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// The request succeeded and there is nothing to return.
    Ack,
    Projects {
        projects: Vec<Project>,
    },
    Project {
        project: Project,
    },
    Workspaces {
        workspaces: Vec<WorkspaceSummary>,
    },
    Workspace {
        workspace: WorkspaceSummary,
    },
    Agents {
        agents: Vec<AgentStatus>,
    },
    Sessions {
        sessions: Vec<Session>,
    },
    SessionMatches {
        matches: Vec<SessionMatch>,
    },
    Session {
        session: Session,
    },
    Transcript {
        entries: Vec<TranscriptEntry>,
    },
    Checkpoints {
        checkpoints: Vec<Checkpoint>,
    },
    Changes {
        changes: Changes,
    },
    Files {
        files: Vec<FileEntry>,
    },
    Commands {
        commands: Vec<SlashCommand>,
    },
    ReviewComments {
        comments: Vec<ReviewComment>,
    },
    Draft {
        text: String,
    },
    Terminal {
        terminal: TerminalId,
    },
    Usage {
        by_day: Vec<UsageRow>,
        by_agent: Vec<UsageRow>,
    },
    /// A commit was made, and this is what it is called.
    Committed {
        commit: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_fields_may_be_omitted_by_a_client_that_does_not_care() {
        // The CLI writes these by hand in its tests and an MCP caller writes
        // them from a schema; neither should have to send explicit nulls.
        let request: Request =
            serde_json::from_str(r#"{"method":"list_workspaces"}"#).expect("omitted option");
        assert_eq!(request, Request::ListWorkspaces { project: None });
    }

    #[test]
    fn a_misspelled_field_is_refused_rather_than_dropped() {
        // Without `deny_unknown_fields` a typo'd `branch` would start a
        // workspace on an empty branch name instead of failing.
        let parsed = serde_json::from_str::<Request>(
            r#"{"method":"create_workspace","project":"comet","branchh":"x"}"#,
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn requests_round_trip() {
        let cases = [
            Request::Ping,
            Request::AddProject {
                path: PathBuf::from("/tmp/comet"),
            },
            Request::StartSession {
                workspace: WorkspaceId("comet/harbor".into()),
                agent: "claude".into(),
                prompt: "write the test first".into(),
                model: Some("opus".into()),
            },
            Request::SessionTranscript {
                session: SessionId("s-1".into()),
                after: Some(10),
                limit: None,
            },
        ];
        for request in cases {
            let text = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        }
    }
}
