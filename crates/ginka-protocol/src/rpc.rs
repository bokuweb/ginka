//! The request/response surface of the daemon.
//!
//! This enum *is* the app's capability list. Anything the UI can do appears
//! here, which is what makes the same operation reachable from `ginka` on the
//! command line and from the MCP server (`AGENTS.md` rule 3). Adding a private
//! path from a view into `ginka-core` is how that guarantee gets lost.

use crate::ids::{AccountId, CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
use crate::model::{
    Account, AgentStatus, Attachment, BranchInfo, ChangeSource, Changes, Checkpoint,
    ConnectorState, ContentMatch, DiffSide, FileContent, FileEntry, GitCommit, PlanSnapshot,
    Project, ReviewComment, Session, SessionMatch, SessionOrigin, Skill, SlashCommand,
    TerminalInfo, TranscriptEntry, UsageRow, WorkspaceContentMatch, WorkspaceFileMatch,
    WorkspaceSummary,
};
use crate::provider::{AccessMode, ProviderKind};
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
    AddProject {
        path: PathBuf,
        /// Optional reader-facing name; the stable key still derives from the path.
        #[serde(default)]
        label: Option<String>,
    },
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
    /// Archive or restore a workspace without deleting its worktree or history.
    ArchiveWorkspace {
        workspace: WorkspaceId,
        archived: bool,
    },

    /// Which agents this machine has, and whether they are usable.
    ///
    /// Probing runs each vendor's CLI, so this is a request rather than
    /// something a client can work out for itself.
    ListAgents,
    /// Every login of every provider, the defaults first, with what the last
    /// probe said about each (`docs/accounts.md` §8).
    Accounts,
    /// Add a login: a directory for the provider's CLI to sign into.
    ///
    /// The directory is created empty; the vendor fills it through
    /// [`Request::LoginAccount`]. Ginka never holds the credential.
    AddAccount {
        /// A slug, immutable once created, unique across providers.
        id: AccountId,
        provider: ProviderKind,
        /// What the chip says.
        label: String,
    },
    /// Forget a login. The directory holds the vendor's sign-in, which is the
    /// thing a person least wants deleted by accident, so it stays unless
    /// `delete_home` says otherwise.
    RemoveAccount { id: AccountId, delete_home: bool },
    /// Run the vendor's own sign-in for an account, in a terminal in
    /// `workspace`'s dock, with the account's directory in its environment.
    LoginAccount {
        id: AccountId,
        workspace: WorkspaceId,
        rows: u16,
        cols: u16,
    },
    /// Ask the provider how much of an account's rate-limit windows is left,
    /// without running a turn. Answers with the reading, and pushes it.
    RefreshPlanUsage { account: AccountId },
    /// Sessions, newest first. `workspace` limits it; `origin` limits it to
    /// the session still answering that thread, which is how a connector
    /// finds where a reply belongs.
    ListSessions {
        workspace: Option<WorkspaceId>,
        #[serde(default)]
        origin: Option<SessionOrigin>,
    },
    /// Start an agent in a workspace and send it `prompt`.
    StartSession {
        workspace: WorkspaceId,
        /// A driver id: `claude`, `codex`, …
        agent: String,
        prompt: String,
        model: Option<String>,
        /// Provider reasoning level, when the selected model accepts one.
        #[serde(default)]
        reasoning_effort: Option<String>,
        /// Provider service tier, when the selected model accepts one.
        #[serde(default)]
        service_tier: Option<String>,
        /// Which login to run on; the provider's default when absent.
        account: Option<AccountId>,
        /// What the agent may do without asking. `Ask` when absent.
        #[serde(default)]
        access_mode: Option<AccessMode>,
        /// Where the conversation came from, when a chat platform is asking.
        /// Recorded on the session and never changed.
        #[serde(default)]
        origin: Option<SessionOrigin>,
    },
    /// Change the provider options used by later turns of a conversation.
    ///
    /// These three fields are the complete desired provider option set;
    /// `null` selects the provider default. The driver decides whether its
    /// transport can keep the vendor session or needs a restart.
    UpdateSessionOptions {
        session: SessionId,
        model: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    },
    /// Ask the same question in several worktrees at once.
    ///
    /// Orca's fan-out: a task with more than one reasonable approach is worth
    /// trying more than once, and comparing three answers costs less than
    /// re-prompting one agent three times. One worktree per attempt, so the
    /// attempts cannot tread on each other.
    FanOut {
        project: ProjectName,
        /// What the branches are called: `<prefix>-1`, `<prefix>-2`, …
        branch_prefix: String,
        base: Option<String>,
        prompt: String,
        /// One attempt per entry. The same agent can appear more than once.
        attempts: Vec<Attempt>,
    },
    /// Send a follow-up. Queued when the agent is mid-turn, which is why this
    /// answers `Ack` rather than waiting for the reply.
    SendMessage { session: SessionId, text: String },
    /// Ask the provider to compact an idle conversation's context.
    ///
    /// This is deliberately distinct from a follow-up: it is refused while a
    /// turn is running and on providers without an explicit compact command.
    CompactSession { session: SessionId },
    /// Answer an [`AskUser`](crate::event::AgentEvent::AskUser), approve a
    /// [`PlanProposal`](crate::event::AgentEvent::PlanProposal), or resolve a
    /// [`Permission`](crate::event::AgentEvent::Permission) request.
    RespondToAgent {
        session: SessionId,
        /// The `id` from the event being answered.
        request_id: String,
        response: String,
    },
    /// Rename a conversation. An empty title clears it back to none.
    RenameSession { session: SessionId, title: String },
    /// Stop a session answering the thread that started it, so the next
    /// message there starts a new one. The origin stays on the record.
    CloseSessionOrigin { session: SessionId },
    /// Forget a session and its transcript and checkpoints.
    RemoveSession { session: SessionId },
    /// Take a copy of a conversation as it was, and carry on from there.
    ///
    /// `after` is the transcript position to fork at; `None` forks the whole
    /// thing. On the same agent and login the fork inherits the vendor's
    /// session id, so continuing it continues the same conversation with the
    /// agent. Naming another `agent`, or another `account`, moves the
    /// conversation instead: the vendor's thread cannot follow, so the fork
    /// is handed a digest of the transcript on its first turn
    /// (`ginka-core::handoff`). `model` applies to the fork; when the agent
    /// changes and no model is named, the new agent's default is used, because
    /// a model id is the vendor's and does not carry across.
    ForkSession {
        session: SessionId,
        after: Option<u64>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        account: Option<AccountId>,
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
    /// Recent commits in a workspace, newest first and bounded by the daemon.
    WorkspaceHistory {
        workspace: WorkspaceId,
        /// Maximum rows to return; defaults to 50 and never exceeds 200.
        #[serde(default)]
        limit: Option<u32>,
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
    /// Fetch and fast-forward a clean workspace branch from its upstream.
    Pull { workspace: WorkspaceId },

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
    /// Run `zg index` for a workspace in a daemon terminal, so agents started
    /// there are handed zvec-grep's search (`ginka-core::tools`). Answers
    /// the terminal; refused when `zg` is not installed.
    IndexWorkspace {
        workspace: WorkspaceId,
        rows: u16,
        cols: u16,
    },
    /// Write a commit message for a workspace's changes on a cheap model
    /// (`docs/roadmap.md` §3.3 N9). Answers `Ack` at once; the message
    /// arrives as `DaemonEvent::CommitMessageGenerated`. `agent` names the
    /// driver to run it on; the workspace's latest session's otherwise, and
    /// `claude` failing that. `staged` describes only what is staged.
    GenerateCommitMessage {
        workspace: WorkspaceId,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        staged: bool,
    },
    /// The local branches of a workspace's repository, with which is checked
    /// out here and which are held by other worktrees.
    ListBranches { workspace: WorkspaceId },
    /// Check a branch out inside a workspace, creating it from HEAD with
    /// `create`. The workspace keeps its name: its id never follows the branch
    /// (`AGENTS.md` rule 4), so nothing is re-keyed.
    CheckoutBranch {
        workspace: WorkspaceId,
        branch: String,
        #[serde(default)]
        create: bool,
    },
    /// The skills the agents can load, across every ecosystem's roots and
    /// every registered project's — or one project's, when named
    /// (`docs/roadmap.md` §3.3 N11).
    ListSkills {
        #[serde(default)]
        project: Option<ProjectName>,
    },
    /// Enable or disable every copy of a skill, by renaming its `SKILL.md`.
    /// Nothing is deleted; `project` narrows the search the same way.
    SetSkillEnabled {
        name: String,
        enabled: bool,
        #[serde(default)]
        project: Option<ProjectName>,
    },
    /// What the user was in the middle of typing in a workspace.
    ComposerDraft { workspace: WorkspaceId },
    /// Keep what they are typing. An empty draft forgets it.
    SaveComposerDraft {
        workspace: WorkspaceId,
        text: String,
    },

    /// What the work has cost, by day, by agent and by account, with the
    /// latest reading of every account's rate-limit windows.
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
    /// Store a file the user attached, so a message can refer to it.
    ///
    /// The bytes travel base64-encoded because the wire is JSON; the daemon
    /// writes them once and every later mention is the reference it answers
    /// with (`docs/roadmap.md` §3.3 N6).
    UploadAttachment { name: String, data_base64: String },
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
    /// The lines in a workspace's files that contain `query`.
    ///
    /// The other half of finding something: a reader who knows what the code
    /// says and not what it is called cannot get there by path.
    SearchContent {
        workspace: WorkspaceId,
        query: String,
        limit: Option<u32>,
    },
    /// Find fuzzy file paths and literal source lines across a project's active workspaces.
    ///
    /// `limit` is shared across the complete result, rather than applied once
    /// per worktree, so one request remains bounded as projects grow.
    SearchProject {
        project: ProjectName,
        query: String,
        limit: Option<u32>,
    },

    /// Read one of a workspace's files, for the panel that shows it.
    ///
    /// Bounded and resolved inside the worktree by the daemon: a client asking
    /// for `../../.ssh/id_rsa` is asking the daemon to do something it will
    /// not do.
    ReadFile {
        workspace: WorkspaceId,
        path: String,
    },
    /// Save an existing text file if it still matches the revision read.
    WriteFile {
        /// Workspace whose worktree contains the file.
        workspace: WorkspaceId,
        /// Path relative to the worktree root.
        path: String,
        /// Complete replacement contents.
        text: String,
        /// The opaque revision returned by [`Request::ReadFile`].
        expected_revision: String,
    },

    /// The shells already running in a workspace.
    ///
    /// A window that has just opened asks this: the daemon kept them running
    /// while it was gone, and a dock that showed none of them would be hiding
    /// work that is still going.
    WorkspaceTerminals { workspace: WorkspaceId },
    /// What a terminal has printed lately, for a window reattaching to it.
    TerminalHistory { terminal: TerminalId },
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

    /// Every chat connector this daemon hosts, and whether each is connected
    /// (`docs/connectors.md` §3.3).
    ListConnectors,
    /// Let one more platform member speak to a connector.
    ///
    /// Written into the settings file, because the allowlist is
    /// configuration; the connector picks the change up at once.
    AllowConnectorSender {
        /// `slack`.
        connector: String,
        /// The platform's own member id, `U01ABC2DEF3` on Slack.
        sender: String,
    },
    /// Post one message into a channel and take it back, to prove the
    /// tokens and the channel are right.
    TestConnector {
        /// `slack`.
        connector: String,
        /// The platform's conversation id.
        channel: String,
    },

    /// Ask the daemon to exit once it has flushed its state.
    Shutdown,
}

/// One arm of a fan-out.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// A driver id: `claude`, `codex`, …
    pub agent: String,
    pub model: Option<String>,
    /// Which login to run on; the provider's default when absent.
    #[serde(default)]
    pub account: Option<AccountId>,
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
    Accounts {
        accounts: Vec<Account>,
    },
    Account {
        account: Account,
    },
    /// A reading of an account's windows, or none when the provider could
    /// not be asked.
    PlanUsage {
        snapshot: Option<PlanSnapshot>,
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
    /// Provider options were applied to this session. When the outcome says a
    /// restart was required, `session` is the context-preserving replacement.
    SessionOptionsApplied {
        session: Session,
        outcome: crate::provider::OptionOutcome,
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
    /// Recent commits in newest-first order.
    History {
        commits: Vec<GitCommit>,
    },
    Files {
        files: Vec<FileEntry>,
    },
    Commands {
        commands: Vec<SlashCommand>,
    },
    Branches {
        branches: Vec<BranchInfo>,
    },
    Skills {
        skills: Vec<Skill>,
        /// The scan stopped at its cap; the list is what fitted.
        truncated: bool,
    },
    ReviewComments {
        comments: Vec<ReviewComment>,
    },
    Attachment {
        attachment: Attachment,
    },
    Draft {
        text: String,
    },
    Terminal {
        terminal: TerminalId,
    },
    FileContent {
        file: FileContent,
    },
    Matches {
        matches: Vec<ContentMatch>,
    },
    /// Path and content hits from more than one worktree, tagged with their source.
    WorkspaceMatches {
        /// Fuzzy path hits, under their own shared project-wide limit.
        files: Vec<WorkspaceFileMatch>,
        /// Literal content hits, under their own shared project-wide limit.
        matches: Vec<WorkspaceContentMatch>,
    },
    Terminals {
        terminals: Vec<TerminalInfo>,
    },
    /// What a fan-out started, and what it could not.
    FannedOut {
        started: Vec<Session>,
        /// One line per attempt that did not start, in the order they were
        /// asked for: an arm that failed must not take the others with it.
        failed: Vec<String>,
    },
    /// What a terminal printed before this window was looking.
    TerminalHistory {
        data: String,
    },
    Usage {
        by_day: Vec<UsageRow>,
        by_agent: Vec<UsageRow>,
        by_account: Vec<UsageRow>,
        /// The latest reading per account, for those that have one.
        plans: Vec<PlanSnapshot>,
    },
    /// A commit was made, and this is what it is called.
    Committed {
        commit: String,
    },
    Connectors {
        connectors: Vec<ConnectorState>,
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
        // A field added after a client was written is one that client never
        // sends, and it must still parse.
        let request: Request = serde_json::from_str(
            r#"{"method":"start_session","workspace":"comet/harbor","agent":"claude",
                "prompt":"go","model":null,"account":null}"#,
        )
        .expect("a start with no origin and no access mode");
        assert!(matches!(
            request,
            Request::StartSession {
                origin: None,
                access_mode: None,
                ..
            }
        ));
    }

    #[test]
    fn a_fork_with_no_destination_stays_on_its_agent() {
        // A client written before a fork could move a conversation still
        // sends a valid fork.
        let request: Request =
            serde_json::from_str(r#"{"method":"fork_session","session":"s-1","after":null}"#)
                .expect("a fork with no destination");
        assert!(matches!(
            request,
            Request::ForkSession {
                agent: None,
                account: None,
                ..
            }
        ));
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
                label: Some("Client website".into()),
            },
            Request::StartSession {
                workspace: WorkspaceId("comet/harbor".into()),
                agent: "claude".into(),
                prompt: "write the test first".into(),
                model: Some("opus".into()),
                reasoning_effort: None,
                service_tier: None,
                account: Some(AccountId("claude-work".into())),
                access_mode: Some(AccessMode::Auto),
                origin: Some(SessionOrigin {
                    connector: "slack".into(),
                    channel: "C1".into(),
                    thread: "1725500000.000100".into(),
                }),
            },
            Request::ListConnectors,
            Request::SessionTranscript {
                session: SessionId("s-1".into()),
                after: Some(10),
                limit: None,
            },
            Request::CompactSession {
                session: SessionId("s-1".into()),
            },
            Request::Pull {
                workspace: WorkspaceId("comet/harbor".into()),
            },
            Request::WorkspaceHistory {
                workspace: WorkspaceId("comet/harbor".into()),
                limit: Some(25),
            },
            Request::ForkSession {
                session: SessionId("s-1".into()),
                after: None,
                agent: Some("codex".into()),
                model: None,
                account: None,
            },
            Request::SetSkillEnabled {
                name: "docx".into(),
                enabled: false,
                project: None,
            },
        ];
        for request in cases {
            let text = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        }
    }
}
