//! The request/response surface of the daemon.
//!
//! This enum *is* the app's capability list. Anything the UI can do appears
//! here, which is what makes the same operation reachable from `ginka` on the
//! command line and from the MCP server (`AGENTS.md` rule 3). Adding a private
//! path from a view into `ginka-core` is how that guarantee gets lost.

use crate::ids::{AccountId, CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
use crate::model::{
    Account, AgentStatus, Attachment, BranchInfo, ChangeSource, Changes, Checkpoint,
    ConnectorState, ContentMatch, DiffSide, FileContent, FileEntry, GitCommit, Note, PlanSnapshot,
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
    /// Name a project the way the reader groups it; blank clears it. The
    /// project's `name` — its identity — does not change.
    SetProjectLabel { project: ProjectName, label: String },
    /// Put a project at `index` in the rail's order; an index past the end
    /// is the end.
    MoveProject { project: ProjectName, index: u32 },

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
    /// Write the line of status shown under a workspace in the sidebar, or
    /// clear it with `None` or a blank note.
    ///
    /// Meant for the agent working there: over MCP it names no workspace and
    /// sends the directory it runs in as `path` instead, which the daemon
    /// resolves to the worktree holding it. One of the two is required.
    SetWorkspaceStatus {
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        #[serde(default)]
        path: Option<std::path::PathBuf>,
        note: Option<String>,
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
    /// Select the account future sessions of its provider use. Existing
    /// sessions retain the account they started on.
    SelectAccount { id: AccountId },
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
        /// Which login to run on; the provider's active account when absent.
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
    /// Follow-ups waiting behind a session's active turn, in dispatch order.
    QueuedMessages {
        /// Session whose pending prompts are requested.
        session: SessionId,
    },
    /// Replace the text of one queued follow-up without changing its place.
    EditQueuedMessage {
        /// Session that owns the queue.
        session: SessionId,
        /// Stable session-local row id.
        id: u64,
        /// Replacement prompt text.
        text: String,
    },
    /// Remove one queued follow-up before it reaches the transcript.
    RemoveQueuedMessage {
        /// Session that owns the queue.
        session: SessionId,
        /// Stable session-local row id.
        id: u64,
    },
    /// Move one queued follow-up to a zero-based dispatch position.
    MoveQueuedMessage {
        /// Session that owns the queue.
        session: SessionId,
        /// Stable session-local row id.
        id: u64,
        /// Destination in dispatch order, clamped to the queue's end.
        index: u32,
    },
    /// Inject one queued follow-up into the active turn when its transport can
    /// receive unsolicited input.
    SendQueuedMessageNow {
        /// Session whose live transport should receive the prompt.
        session: SessionId,
        /// Stable session-local row id.
        id: u64,
    },
    /// Put a follow-up in the queue even where the running turn could take
    /// it now — the Codex CLI's Tab. With nothing running and nothing
    /// waiting ahead of it, it starts at once, like `SendMessage`.
    QueueMessage { session: SessionId, text: String },
    /// Stop the running turn and send this queued prompt next — opencodex's
    /// Steer. The rest of the queue keeps its order and keeps going. With
    /// nothing running it is sent at once.
    InterruptWithQueuedMessage { session: SessionId, id: u64 },
    /// Hold the queue, or let it go. A stopped or failed turn holds it; a
    /// restarted daemon brings it back held. Letting a held queue go while
    /// nothing is running sends its first prompt.
    SetQueuePaused { session: SessionId, paused: bool },
    /// Throw away every waiting follow-up.
    ClearQueue { session: SessionId },
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
    /// Edit a prompt already sent and run the conversation again from it —
    /// MonoCode's edit-and-resend.
    ///
    /// `seq` names the prompt's transcript entry. The worktree goes back to
    /// the checkpoint taken before that prompt's turn (snapshotting what is
    /// there first, so the rewind itself can be undone), and a new session
    /// takes the conversation up to just before it — on a fresh vendor
    /// thread, handed a digest, because the old thread has the answers being
    /// replaced in it. `text` is then sent there. The original conversation
    /// is left as it was. Answers with the new session.
    EditPrompt {
        session: SessionId,
        seq: u64,
        text: String,
    },
    /// The conversations the agents' own CLIs keep for a workspace's
    /// directory — `claude` or `codex` run in a terminal, outside Ginka —
    /// newest first, leaving out the ones Ginka's sessions already hold.
    /// Answers with [`Response::CliSessions`].
    CliSessions { workspace: WorkspaceId },
    /// Bring one of those conversations in: a new session in `workspace`
    /// holding the vendor's id, so its next turn resumes that same thread,
    /// with the conversation's recent turns as its transcript. On the
    /// provider's system login, which is where the CLI keeps it. Answers
    /// with the new session.
    AdoptCliSession {
        workspace: WorkspaceId,
        agent: String,
        vendor_session_id: String,
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
        /// Unchanged lines around each edit; omitted means three, at most 25.
        #[serde(default)]
        context_lines: Option<u8>,
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
    ///
    /// `amend` folds the work into the last commit instead, keeping its
    /// message when `message` is empty; the daemon refuses it when that
    /// commit is already on a remote.
    Commit {
        workspace: WorkspaceId,
        message: String,
        all: bool,
        #[serde(default)]
        amend: bool,
    },
    /// Merge a workspace's branch into another — fan-out's "merge the
    /// winner". `into` defaults to the branch the project's own checkout is
    /// on. Uncommitted work in the workspace is committed first with
    /// `message`, and refused without one.
    MergeWorkspace {
        workspace: WorkspaceId,
        into: Option<String>,
        message: Option<String>,
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
    /// Move one exact diff hunk into or out of the index.
    ///
    /// `header` is the complete `@@` line last read from the corresponding
    /// unstaged or staged diff. A stale header is refused rather than matched
    /// approximately to another change.
    StageHunk {
        workspace: WorkspaceId,
        path: String,
        header: String,
        /// `true` stages it, `false` takes it back out.
        staged: bool,
    },
    /// Throw away one exact unstaged hunk.
    ///
    /// `header` is the complete `@@` line last read from the unstaged diff. A
    /// stale header is refused rather than matched approximately.
    RevertHunk {
        workspace: WorkspaceId,
        path: String,
        header: String,
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
    ///
    /// `force_with_lease` replaces rewritten history on the remote — after
    /// an amend or a rebase — only if the remote is still what was last
    /// fetched. It is never what a plain push falls back to.
    Push {
        workspace: WorkspaceId,
        #[serde(default)]
        force_with_lease: bool,
    },
    /// Fetch and fast-forward a clean workspace branch from its upstream.
    Pull { workspace: WorkspaceId },
    /// Bring a workspace branch level with its remote in one action: publish
    /// it if it was never pushed, otherwise fast-forward, then push what is
    /// ahead. Answers with [`Response::Synced`].
    Sync { workspace: WorkspaceId },
    /// Push the workspace's branch and open a pull request for it with the
    /// GitHub CLI, titled and described from its commits. Answers with the
    /// pull request's address — the existing one's, when the branch already
    /// has one open.
    ///
    /// `gh` rather than the API: it is already signed in wherever someone
    /// reviews on GitHub, and Ginka never holds the token (rule 7).
    CreatePullRequest {
        workspace: WorkspaceId,
        #[serde(default)]
        draft: bool,
    },

    /// The notes of a project, or every note when `project` is absent, most
    /// recently touched first.
    ListNotes {
        #[serde(default)]
        project: Option<ProjectName>,
        /// Case-insensitive substring in title, body or tags.
        #[serde(default)]
        query: Option<String>,
        /// Exact tag, compared without case.
        #[serde(default)]
        tag: Option<String>,
    },
    /// Write a note. Without an `id` it is a new one; with one it replaces the
    /// title and body of the note that has it.
    SaveNote {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        project: Option<ProjectName>,
        title: String,
        body: String,
        /// Omit to retain existing tags on edit; an empty list clears them.
        #[serde(default)]
        tags: Option<Vec<String>>,
    },
    /// Forget a note.
    RemoveNote { id: String },

    /// Raise a ticket: work noticed in passing, handed to the reader as a
    /// card they can start in its own session. `workspace` defaults to the
    /// raising session's; one of the two is needed.
    RaiseTicket {
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        #[serde(default)]
        from_session: Option<SessionId>,
        title: String,
        #[serde(default)]
        summary: String,
        prompt: String,
    },
    /// Tickets, newest first: a workspace's, or every one. Open ones only
    /// unless `all`.
    ListTickets {
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        #[serde(default)]
        all: bool,
    },
    /// Start a session from an open ticket, with its prompt. In the ticket's
    /// workspace, or in a new worktree on `branch` in the same project.
    /// `agent` defaults to the raising session's agent.
    StartTicket {
        ticket: String,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        branch: Option<String>,
    },
    /// Decide against an open ticket.
    DismissTicket { ticket: String },
    /// Send a follow-up from one session to another. The receiver is told
    /// who sent it and how to answer, which is what makes it a conversation
    /// between agents rather than an anonymous prompt.
    MessageSession {
        from: SessionId,
        to: SessionId,
        text: String,
    },

    /// Saved commands and prompts: a project's own and the global ones, or
    /// only the global ones when `project` is absent. By name.
    ListQuickCommands {
        #[serde(default)]
        project: Option<ProjectName>,
    },
    /// Save a quick command: new without an `id`, a replacement with one.
    SaveQuickCommand {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        project: Option<ProjectName>,
        name: String,
        kind: crate::model::QuickCommandKind,
        body: String,
    },
    /// Forget a quick command.
    RemoveQuickCommand { id: String },
    /// Run a shell quick command in a new terminal in the workspace, which
    /// stays open on a shell afterwards so its output can be read. A prompt
    /// command is refused: sending a prompt is the conversation's to do,
    /// through `SendMessage` or `StartSession`.
    RunQuickCommand {
        workspace: WorkspaceId,
        id: String,
        rows: u16,
        cols: u16,
    },

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
    /// Create a skill in the shared user or registered project's `.agents/skills` root.
    CreateSkill {
        /// Lowercase slug for the new skill directory.
        name: String,
        /// One-line front matter summary.
        description: String,
        /// Markdown instructions written to `SKILL.md`.
        body: String,
        /// Registered project, or the user's home when omitted.
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

    /// Hand a worktree's conflicts — a merge, rebase or cherry-pick that
    /// stopped on them — to an agent to resolve and finish: Orca's "Resolve
    /// with AI". Refused when nothing is conflicted. Answers with the
    /// conversation the request went to.
    ResolveConflicts {
        workspace: WorkspaceId,
        /// Which agent resolves them. Omitted, the workspace's latest
        /// conversation takes them as a follow-up (queued if it is working);
        /// named, or with no conversation yet, a new one starts on it.
        #[serde(default)]
        agent: Option<String>,
    },
    /// Hand a refused commit to an agent to fix — Orca's "Fix with AI":
    /// `output` is what git and its hooks said (the `Commit` error), and
    /// `message` the message it was going to use. The daemon adds the staged
    /// files. Routed like [`Request::ResolveConflicts`].
    FixCommitFailure {
        workspace: WorkspaceId,
        message: String,
        output: String,
        #[serde(default)]
        agent: Option<String>,
    },
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
    /// Read a daemon-owned attachment as a bounded, signature-checked image.
    /// Returns no image for missing, unsafe, oversized or non-image references.
    ReadAttachmentImage { reference: String },
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
    /// Launch a daemon-host editor on an existing file inside the worktree.
    /// `line` is one-based; an external client must expect the editor to open
    /// on the daemon's machine, not on its own.
    OpenExternalEditor {
        /// Workspace whose worktree contains the file.
        workspace: WorkspaceId,
        /// Path relative to the worktree root.
        path: String,
        /// Optional one-based line to focus.
        line: Option<u32>,
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

    /// Scheduled jobs: a project's, or every project's.
    ListCronJobs { project: Option<ProjectName> },
    /// Save a scheduled job: new without an `id`, a replacement with one.
    /// The cron expression or `@once` timestamp is checked here, so a job
    /// that can never fire is refused when written rather than found silent.
    SaveCronJob {
        id: Option<i64>,
        project: ProjectName,
        workspace: Option<WorkspaceId>,
        name: String,
        schedule: String,
        via: crate::model::CronVia,
        agent: Option<String>,
        body: String,
        /// Run first on every scheduled firing; a failure skips the firing.
        #[serde(default)]
        precheck: Option<String>,
        enabled: bool,
    },
    /// Forget a scheduled job and its history.
    RemoveCronJob { id: i64 },
    /// Fire a job now, as its schedule would — including skipping it while
    /// its previous run is still going. Its precheck is not run: asking for
    /// a run now is the answer the probe would have given.
    RunCronJob { id: i64 },
    /// A job's firings, most recent first.
    CronRuns { id: i64, limit: Option<u32> },

    /// The last `limit` transcript entries before position `before` — or
    /// before the end — oldest first. How a window opens a long session on
    /// its latest page and reads the earlier ones on request.
    SessionTranscriptTail {
        session: SessionId,
        before: Option<u64>,
        limit: u32,
    },

    /// The daemon's settings (`settings.json`) as JSON, with every
    /// environment value replaced by `[set]`: the names are the reader's to
    /// see, the values — keys and tokens — are not the wire's to carry.
    DaemonSettings,
    /// Change one top-level setting to a JSON value, checked against the
    /// settings' shape before it is written and taken at once.
    UpdateDaemonSettings { key: String, value: String },
    /// The providers shipped by this build and their daemon-owned settings.
    ListProviderSettings,
    /// Change one provider without replacing other providers' settings.
    UpdateProviderSettings {
        provider: ProviderKind,
        /// Leave the enabled state unchanged when absent.
        enabled: Option<bool>,
        /// Set a CLI path, or leave it unchanged when absent.
        program: Option<String>,
        /// Remove a CLI path override and use the driver's default binary.
        clear_program: bool,
    },

    /// A page the browser surface finished loading in a workspace, for the
    /// address bar to complete from. Credentials, fragments and
    /// secret-looking query parameters are dropped before it is kept.
    RecordBrowserVisit {
        workspace: WorkspaceId,
        url: String,
        title: Option<String>,
    },
    /// The pages to offer for what is typed in a workspace's address bar,
    /// most frecent first.
    BrowserSuggestions {
        workspace: WorkspaceId,
        query: String,
        limit: Option<u32>,
    },
    /// Put the skills that teach an agent to drive Ginka — `ginka-start`,
    /// `ginka-chat`, `ginka-terminal`, `ginka-loop` — into Claude Code's and
    /// Codex's skills directories on the daemon's host. A different file
    /// already there is left alone unless `force`.
    InstallBundledSkills { force: bool },

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
    /// Which login to run on; the provider's active account when absent.
    #[serde(default)]
    pub account: Option<AccountId>,
}

/// One shipped provider's runtime configuration, excluding environment secrets.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSetting {
    /// The provider whose driver this build ships.
    pub provider: ProviderKind,
    /// Whether new turns may use it.
    pub enabled: bool,
    /// CLI path override, or the driver's default command when absent.
    pub program: Option<String>,
}

/// What the daemon answers with.
///
/// One variant per shape rather than per request: several requests legitimately
/// answer `Ack`, and a client that has to match on the request it sent to
/// understand the reply is a client that cannot be written generically.
///
/// Variants differ widely in size and that is allowed: a response is built
/// once, serialized and dropped, so boxing the large ones would cost every
/// construction and match site for memory no one holds.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
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
    QueuedMessages {
        /// Pending prompts in dispatch order.
        messages: Vec<crate::model::QueuedMessage>,
        /// Whether the active transport can receive a waiting prompt now.
        can_send_now: bool,
        /// Whether the queue is held: nothing dispatches until it is let go.
        #[serde(default)]
        paused: bool,
        /// Unix seconds at which a hold a usage limit placed lets itself go;
        /// absent for a hold a person placed, or none.
        #[serde(default)]
        resume_at: Option<i64>,
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
    /// A previewable attachment, or no image if it cannot be safely displayed.
    AttachmentImage {
        image: Option<crate::model::FileImage>,
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
        /// When the rate table that priced unpriced turns was fetched; absent
        /// when there is none, and nothing was estimated.
        #[serde(default)]
        rates_fetched_at: Option<i64>,
        /// What each model cost, including agents run outside Ginka.
        #[serde(default)]
        by_model: Vec<UsageRow>,
        /// What each project cost, including agents run in its folders
        /// outside Ginka.
        #[serde(default)]
        by_project: Vec<UsageRow>,
        /// The latest reading per account, for those that have one.
        plans: Vec<PlanSnapshot>,
    },
    /// A commit was made, and this is what it is called.
    /// What a merge did.
    Merged {
        outcome: crate::model::MergeOutcome,
    },
    Committed {
        commit: String,
    },
    Connectors {
        connectors: Vec<ConnectorState>,
    },
    /// A pull request exists for the branch, and this is where.
    PullRequest {
        url: String,
    },
    /// The conversations [`Request::CliSessions`] found.
    CliSessions {
        sessions: Vec<crate::model::CliSession>,
    },
    /// What [`Request::Sync`] did.
    Synced {
        /// Commits came in from the remote.
        pulled: bool,
        /// Commits went out, or the branch was published.
        pushed: bool,
    },
    Notes {
        notes: Vec<Note>,
    },
    Note {
        note: Note,
    },
    /// The daemon's settings, secrets left out.
    DaemonSettings {
        json: String,
    },
    /// Runtime provider choices, including disabled providers.
    ProviderSettings {
        providers: Vec<ProviderSetting>,
    },
    /// Pages the address bar can offer.
    BrowserSuggestions {
        pages: Vec<crate::model::VisitedPage>,
    },
    /// What installing Ginka's own skills did, a row per skill and place.
    BundledSkillsInstalled {
        results: Vec<crate::model::BundledSkillInstall>,
    },
    CronJobs {
        jobs: Vec<crate::model::CronJob>,
    },
    CronJob {
        job: crate::model::CronJob,
    },
    CronRuns {
        runs: Vec<crate::model::CronRun>,
    },
    Tickets {
        tickets: Vec<crate::model::Ticket>,
    },
    Ticket {
        ticket: crate::model::Ticket,
    },
    QuickCommands {
        commands: Vec<crate::model::QuickCommand>,
    },
    QuickCommand {
        command: crate::model::QuickCommand,
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
            Request::EditQueuedMessage {
                session: SessionId("s-1".into()),
                id: 7,
                text: "use the parser".into(),
            },
            Request::MoveQueuedMessage {
                session: SessionId("s-1".into()),
                id: 7,
                index: 0,
            },
            Request::SendQueuedMessageNow {
                session: SessionId("s-1".into()),
                id: 7,
            },
            Request::Pull {
                workspace: WorkspaceId("comet/harbor".into()),
            },
            Request::Sync {
                workspace: WorkspaceId("comet/harbor".into()),
            },
            Request::CliSessions {
                workspace: WorkspaceId("comet/harbor".into()),
            },
            Request::AdoptCliSession {
                workspace: WorkspaceId("comet/harbor".into()),
                agent: "claude".into(),
                vendor_session_id: "c-1".into(),
            },
            Request::WorkspaceHistory {
                workspace: WorkspaceId("comet/harbor".into()),
                limit: Some(25),
            },
            Request::StageHunk {
                workspace: WorkspaceId("comet/harbor".into()),
                path: "src/main.rs".into(),
                header: "@@ -1 +1 @@".into(),
                staged: true,
            },
            Request::SelectAccount {
                id: AccountId("codex-work".into()),
            },
            Request::RevertHunk {
                workspace: WorkspaceId("comet/harbor".into()),
                path: "src/main.rs".into(),
                header: "@@ -1 +1 @@".into(),
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
            Request::CreateSkill {
                name: "release-notes".into(),
                description: "Write release notes".into(),
                body: "Summarize changes.".into(),
                project: Some(ProjectName("comet".into())),
            },
        ];
        for request in cases {
            let text = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        }
    }
}
