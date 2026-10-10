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
    WorkspaceFolder, WorkspaceSummary,
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
        /// The repository or folder to register. A daemon-host path.
        path: PathBuf,
        /// Optional reader-facing name; the stable key still derives from the path.
        #[serde(default)]
        label: Option<String>,
    },
    /// Forget a project. The worktrees on disk are left alone: the daemon
    /// registered them, it did not create the user's code.
    RemoveProject {
        /// The project to forget.
        project: ProjectName,
    },
    /// Name a project the way the reader groups it; blank clears it. The
    /// project's `name` — its identity — does not change.
    SetProjectLabel {
        /// The project to name.
        project: ProjectName,
        /// The new label; blank clears it.
        label: String,
    },
    /// Put a project at `index` in the rail's order; an index past the end
    /// is the end.
    MoveProject {
        /// The project to move.
        project: ProjectName,
        /// Zero-based destination in the rail's order.
        index: u32,
    },

    /// Every workspace, reconciled against git first. `project` limits it.
    ListWorkspaces {
        /// Only this project's workspaces, when set.
        project: Option<ProjectName>,
    },
    /// Assigned sidebar folders and member counts in one registered project.
    /// This reads persisted metadata without polling git or changing state.
    ListWorkspaceFolders {
        /// The project whose catalog to read, including archived-only folders.
        project: ProjectName,
    },
    /// Create a worktree on `branch`, cutting it from `base` when the branch
    /// does not exist yet.
    CreateWorkspace {
        /// The project to add the worktree to.
        project: ProjectName,
        /// The branch to check out; also names the worktree.
        branch: String,
        /// What a new branch is cut from; the project's default branch when absent.
        base: Option<String>,
    },
    /// Make somewhere to work with no project at all.
    ///
    /// The daemon creates a dated directory under its own state, registers it
    /// as a plain project and answers with its workspace. This is the "just
    /// start an agent" flow: a question that needs a scratch directory should
    /// not need a repository first.
    CreateScratchWorkspace {
        /// What the directory and project are called; `scratch` when absent.
        name: Option<String>,
    },
    /// Remove a workspace's worktree. `force` is required when it is dirty.
    RemoveWorkspace {
        /// The workspace to remove.
        workspace: WorkspaceId,
        /// Remove it even with uncommitted work, which is then lost.
        force: bool,
    },
    /// Assign a project-local sidebar folder without moving files.
    SetWorkspaceFolder {
        /// The immutable workspace id to organize.
        workspace: WorkspaceId,
        /// A trimmed name of at most 80 characters; blank or `None` clears it.
        folder: Option<String>,
    },
    /// Assign a sidebar folder to a batch, committing all targets or none.
    SetWorkspaceFolders {
        /// Every target must belong to this registered project.
        project: ProjectName,
        /// Between 1 and 256 immutable ids; duplicates are updated once.
        workspaces: Vec<WorkspaceId>,
        /// Same name rules as `SetWorkspaceFolder`; blank or `None` clears it.
        folder: Option<String>,
    },
    /// Pin or unpin a workspace.
    PinWorkspace {
        /// The workspace to pin or unpin.
        workspace: WorkspaceId,
        /// `true` pins it, `false` unpins it.
        pinned: bool,
    },
    /// Pin or unpin a project-local batch, committing all targets or none.
    PinWorkspaces {
        /// Every target must belong to this registered project.
        project: ProjectName,
        /// Between 1 and 256 immutable ids; duplicates are updated once.
        workspaces: Vec<WorkspaceId>,
        /// Set every target to this state, including already matching rows.
        pinned: bool,
    },
    /// Archive or restore a project-local batch, committing all targets or none.
    ArchiveWorkspaces {
        /// Every target must belong to this registered project.
        project: ProjectName,
        /// Between 1 and 256 immutable ids; duplicates are updated once.
        workspaces: Vec<WorkspaceId>,
        /// Set every target to this state, including already matching rows.
        archived: bool,
    },
    /// Archive or restore a workspace without deleting its worktree or history.
    ArchiveWorkspace {
        /// The workspace to archive or restore.
        workspace: WorkspaceId,
        /// `true` archives it, `false` restores it.
        archived: bool,
    },
    /// Write the line of status shown under a workspace in the sidebar, or
    /// clear it with `None` or a blank note.
    ///
    /// Meant for the agent working there: over MCP it names no workspace and
    /// sends the directory it runs in as `path` instead, which the daemon
    /// resolves to the worktree holding it. One of the two is required.
    SetWorkspaceStatus {
        /// The workspace to write on; required unless `path` is given.
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        /// A directory inside the worktree, resolved by the daemon. A daemon-host path.
        #[serde(default)]
        path: Option<std::path::PathBuf>,
        /// The line to show, kept to one bounded line; `None` or blank clears it.
        note: Option<String>,
    },

    /// Ask npm for the latest release of each installed agent CLI and say
    /// which are behind (MonoCode's update check). Only when asked: Ginka
    /// does not phone home. Answers `Ack`; the result arrives as
    /// `DaemonEvent::AgentUpdatesChecked`. Nothing is installed.
    CheckAgentUpdates,
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
        /// The provider whose CLI signs into it.
        provider: ProviderKind,
        /// What the chip says.
        label: String,
    },
    /// Forget a login. The directory holds the vendor's sign-in, which is the
    /// thing a person least wants deleted by accident, so it stays unless
    /// `delete_home` says otherwise.
    RemoveAccount {
        /// The account to forget; a provider's default cannot be removed.
        id: AccountId,
        /// Also delete the account's directory and the sign-in inside it.
        delete_home: bool,
    },
    /// Select the account future sessions of its provider use. Existing
    /// sessions retain the account they started on.
    SelectAccount {
        /// The account new sessions of its provider will use.
        id: AccountId,
    },
    /// Run the vendor's own sign-in for an account, in a terminal in
    /// `workspace`'s dock, with the account's directory in its environment.
    LoginAccount {
        /// The account to sign in.
        id: AccountId,
        /// The workspace whose terminal dock the sign-in opens in.
        workspace: WorkspaceId,
        /// Terminal height in character cells.
        rows: u16,
        /// Terminal width in character cells.
        cols: u16,
    },
    /// Ask the provider how much of an account's rate-limit windows is left,
    /// without running a turn. Answers with the reading, and pushes it.
    RefreshPlanUsage {
        /// The account to read.
        account: AccountId,
    },
    /// Sessions, newest first. `workspace` limits it; `origin` limits it to
    /// the session still answering that thread, which is how a connector
    /// finds where a reply belongs.
    ListSessions {
        /// Only this workspace's sessions, when set.
        workspace: Option<WorkspaceId>,
        /// Only the session still answering this chat thread, when set.
        #[serde(default)]
        origin: Option<SessionOrigin>,
    },
    /// Start an agent in a workspace and send it `prompt`.
    StartSession {
        /// The workspace the agent runs in.
        workspace: WorkspaceId,
        /// A driver id: `claude`, `codex`, …
        agent: String,
        /// The opening prompt, attachment references included.
        prompt: String,
        /// Provider model id; the provider's default when absent.
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
        /// The conversation to change.
        session: SessionId,
        /// Provider model id; `None` selects the provider default.
        model: Option<String>,
        /// Provider reasoning level; `None` selects the model's default.
        reasoning_effort: Option<String>,
        /// Provider service tier; `None` selects the model's default.
        service_tier: Option<String>,
    },
    /// Ask the same question in several worktrees at once.
    ///
    /// Orca's fan-out: a task with more than one reasonable approach is worth
    /// trying more than once, and comparing three answers costs less than
    /// re-prompting one agent three times. One worktree per attempt, so the
    /// attempts cannot tread on each other.
    FanOut {
        /// The project every attempt's worktree is created in.
        project: ProjectName,
        /// What the branches are called: `<prefix>-1`, `<prefix>-2`, …
        branch_prefix: String,
        /// What the branches are cut from; the project's default branch when absent.
        base: Option<String>,
        /// The prompt every attempt is sent.
        prompt: String,
        /// One attempt per entry. The same agent can appear more than once.
        attempts: Vec<Attempt>,
    },
    /// Send a follow-up. Queued when the agent is mid-turn, which is why this
    /// answers `Ack` rather than waiting for the reply.
    SendMessage {
        /// The conversation to continue.
        session: SessionId,
        /// The follow-up prompt, attachment references included.
        text: String,
    },
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
    QueueMessage {
        /// The conversation to queue for.
        session: SessionId,
        /// The follow-up prompt, attachment references included.
        text: String,
    },
    /// Stop the running turn and send this queued prompt next — opencodex's
    /// Steer. The rest of the queue keeps its order and keeps going. With
    /// nothing running it is sent at once.
    InterruptWithQueuedMessage {
        /// The conversation whose turn is stopped.
        session: SessionId,
        /// Stable session-local row id of the prompt to send next.
        id: u64,
    },
    /// Hold the queue, or let it go. A stopped or failed turn holds it; a
    /// restarted daemon brings it back held. Letting a held queue go while
    /// nothing is running sends its first prompt.
    SetQueuePaused {
        /// The conversation whose queue to hold or release.
        session: SessionId,
        /// `true` holds the queue, `false` lets it go.
        paused: bool,
    },
    /// Throw away every waiting follow-up.
    ClearQueue {
        /// The conversation whose queue is emptied.
        session: SessionId,
    },
    /// Ask the provider to compact an idle conversation's context.
    ///
    /// This is deliberately distinct from a follow-up: it is refused while a
    /// turn is running and on providers without an explicit compact command.
    CompactSession {
        /// The idle conversation to compact.
        session: SessionId,
    },
    /// Answer an [`AskUser`](crate::event::AgentEvent::AskUser), approve a
    /// [`PlanProposal`](crate::event::AgentEvent::PlanProposal), or resolve a
    /// [`Permission`](crate::event::AgentEvent::Permission) request.
    RespondToAgent {
        /// The session whose running turn is waiting.
        session: SessionId,
        /// The `id` from the event being answered.
        request_id: String,
        /// The answer: an option's text, free text, or the approval decision
        /// — or, for a card with structured questions, what
        /// [`crate::question::Answers::encode`] wrote. Words answer every
        /// question asked.
        response: String,
    },
    /// Rename a conversation. An empty title clears it back to none.
    RenameSession {
        /// The conversation to rename.
        session: SessionId,
        /// The new title; empty clears it.
        title: String,
    },
    /// Stop a session answering the thread that started it, so the next
    /// message there starts a new one. The origin stays on the record.
    CloseSessionOrigin {
        /// The session to detach from its thread.
        session: SessionId,
    },
    /// Forget a session and its transcript and checkpoints.
    RemoveSession {
        /// The session to forget.
        session: SessionId,
    },
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
        /// The conversation to copy.
        session: SessionId,
        /// Last transcript position kept; `None` keeps the whole transcript.
        after: Option<u64>,
        /// Driver id to continue on; the original's when absent.
        #[serde(default)]
        agent: Option<String>,
        /// Model for the fork; see above for the default when absent.
        #[serde(default)]
        model: Option<String>,
        /// Login to continue on; the original's when absent.
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
        /// The conversation the prompt was sent in.
        session: SessionId,
        /// Transcript position of the user prompt being edited.
        seq: u64,
        /// The replacement prompt.
        text: String,
    },
    /// The conversations the agents' own CLIs keep for a workspace's
    /// directory — `claude` or `codex` run in a terminal, outside Ginka —
    /// newest first, leaving out the ones Ginka's sessions already hold.
    /// Answers with [`Response::CliSessions`].
    CliSessions {
        /// The workspace whose directory the CLIs are asked about.
        workspace: WorkspaceId,
    },
    /// Bring one of those conversations in: a new session in `workspace`
    /// holding the vendor's id, so its next turn resumes that same thread,
    /// with the conversation's recent turns as its transcript. On the
    /// provider's system login, which is where the CLI keeps it. Answers
    /// with the new session.
    AdoptCliSession {
        /// The workspace the new session runs in.
        workspace: WorkspaceId,
        /// Driver id that can resume it: `claude` or `codex`.
        agent: String,
        /// The vendor's session id, from [`Response::CliSessions`].
        vendor_session_id: String,
    },
    /// Find transcript entries containing `query`.
    SearchSessions {
        /// Only this workspace's sessions, when set.
        workspace: Option<WorkspaceId>,
        /// Text to look for in transcript entries.
        query: String,
        /// Maximum matches; 50 when absent.
        limit: Option<u32>,
    },

    /// Kill the agent's process tree.
    CancelSession {
        /// The session whose process is killed.
        session: SessionId,
    },
    /// Read a transcript. `after` pages forward from a sequence number.
    SessionTranscript {
        /// The conversation to read.
        session: SessionId,
        /// Exclusive lower bound on `seq`; from the start when absent.
        after: Option<u64>,
        /// Maximum entries; unbounded when absent.
        limit: Option<u32>,
    },

    /// What has changed in a workspace, against `source`.
    ///
    /// The review half of the loop: an agent that edited twelve files is only
    /// useful if the twelve are readable without leaving the window.
    WorkspaceChanges {
        /// The workspace to diff.
        workspace: WorkspaceId,
        /// What to measure against.
        source: ChangeSource,
        /// Unchanged lines around each edit; omitted means three, at most 25.
        #[serde(default)]
        context_lines: Option<u8>,
    },
    /// Recent commits in a workspace, newest first and bounded by the daemon.
    WorkspaceHistory {
        /// The workspace whose history to read.
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
        /// The workspace to commit in.
        workspace: WorkspaceId,
        /// The commit message; may be empty only with `amend`.
        message: String,
        /// Stage every change, untracked files included, before committing.
        all: bool,
        /// Fold into the last commit instead of making a new one.
        #[serde(default)]
        amend: bool,
    },
    /// Merge a workspace's branch into another — fan-out's "merge the
    /// winner". `into` defaults to the branch the project's own checkout is
    /// on. Uncommitted work in the workspace is committed first with
    /// `message`, and refused without one.
    MergeWorkspace {
        /// The workspace whose branch is merged.
        workspace: WorkspaceId,
        /// The branch to merge into; the project checkout's branch when absent.
        into: Option<String>,
        /// Commit message for uncommitted work, required when there is any.
        message: Option<String>,
    },
    /// Put one file into the next commit, or take it back out.
    ///
    /// Per file, because "these three are right and that one is not" is the
    /// ordinary outcome of reading an agent's work, and committing all of it
    /// is not the only thing a reader may want to do about it.
    StageFile {
        /// The workspace whose index changes.
        workspace: WorkspaceId,
        /// Relative to the worktree root.
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
        /// The workspace whose index changes.
        workspace: WorkspaceId,
        /// Relative to the worktree root.
        path: String,
        /// The hunk's complete `@@` line, as last read.
        header: String,
        /// `true` stages it, `false` takes it back out.
        staged: bool,
    },
    /// Throw away one exact unstaged hunk.
    ///
    /// `header` is the complete `@@` line last read from the unstaged diff. A
    /// stale header is refused rather than matched approximately.
    RevertHunk {
        /// The workspace whose worktree changes.
        workspace: WorkspaceId,
        /// Relative to the worktree root.
        path: String,
        /// The hunk's complete `@@` line, as last read from the unstaged diff.
        header: String,
    },
    /// Throw away one file's uncommitted work.
    ///
    /// Destructive and not undoable through git: a file the agent invented is
    /// deleted. The caller confirms; the daemon does as it is told.
    RevertFile {
        /// The workspace whose worktree changes.
        workspace: WorkspaceId,
        /// Relative to the worktree root.
        path: String,
    },
    /// Push a workspace's branch, setting an upstream if it has none.
    ///
    /// `force_with_lease` replaces rewritten history on the remote — after
    /// an amend or a rebase — only if the remote is still what was last
    /// fetched. It is never what a plain push falls back to.
    Push {
        /// The workspace whose branch is pushed.
        workspace: WorkspaceId,
        /// Replace rewritten remote history, guarded by the last fetch.
        #[serde(default)]
        force_with_lease: bool,
    },
    /// Fetch and fast-forward a clean workspace branch from its upstream.
    Pull {
        /// The workspace to fast-forward.
        workspace: WorkspaceId,
    },
    /// Bring a workspace branch level with its remote in one action: publish
    /// it if it was never pushed, otherwise fast-forward, then push what is
    /// ahead. Answers with [`Response::Synced`].
    Sync {
        /// The workspace to bring level with its remote.
        workspace: WorkspaceId,
    },
    /// Push the workspace's branch and open a pull request for it with the
    /// GitHub CLI, titled and described from its commits. Answers with the
    /// pull request's address — the existing one's, when the branch already
    /// has one open.
    ///
    /// `gh` rather than the API: it is already signed in wherever someone
    /// reviews on GitHub, and Ginka never holds the token (rule 7).
    CreatePullRequest {
        /// The workspace whose branch the pull request is from.
        workspace: WorkspaceId,
        /// Open it as a draft.
        #[serde(default)]
        draft: bool,
    },

    /// The notes of a project, or every note when `project` is absent, most
    /// recently touched first.
    ListNotes {
        /// Only this project's notes, when set.
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
        /// The note to replace; absent creates a new one.
        #[serde(default)]
        id: Option<String>,
        /// The project it belongs to; absent for a note that belongs to none.
        #[serde(default)]
        project: Option<ProjectName>,
        /// One line, shown in the list.
        title: String,
        /// Markdown.
        body: String,
        /// Omit to retain existing tags on edit; an empty list clears them.
        #[serde(default)]
        tags: Option<Vec<String>>,
    },
    /// Forget a note.
    RemoveNote {
        /// The note to forget.
        id: String,
    },

    /// Raise a ticket: work noticed in passing, handed to the reader as a
    /// card they can start in its own session. `workspace` defaults to the
    /// raising session's; one of the two is needed.
    RaiseTicket {
        /// Where the ticket belongs; the raising session's workspace when absent.
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        /// The session raising it, when an agent is.
        #[serde(default)]
        from_session: Option<SessionId>,
        /// A short imperative, the card's heading.
        title: String,
        /// One or two sentences for the card: why now, and what it will do.
        #[serde(default)]
        summary: String,
        /// What the new session is told; self-contained.
        prompt: String,
    },
    /// Tickets, newest first: a workspace's, or every one. Open ones only
    /// unless `all`.
    ListTickets {
        /// Only this workspace's tickets, when set.
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        /// Include started and dismissed tickets.
        #[serde(default)]
        all: bool,
    },
    /// Start a session from an open ticket, with its prompt. In the ticket's
    /// workspace, or in a new worktree on `branch` in the same project.
    /// `agent` defaults to the raising session's agent.
    StartTicket {
        /// The open ticket to start.
        ticket: String,
        /// Driver id to run it on.
        #[serde(default)]
        agent: Option<String>,
        /// A branch for a new worktree; the ticket's workspace when absent.
        #[serde(default)]
        branch: Option<String>,
    },
    /// Decide against an open ticket.
    DismissTicket {
        /// The open ticket to dismiss.
        ticket: String,
    },
    /// Send a follow-up from one session to another. The receiver is told
    /// who sent it and how to answer, which is what makes it a conversation
    /// between agents rather than an anonymous prompt.
    MessageSession {
        /// The session sending; named to the receiver so it can answer.
        from: SessionId,
        /// The session receiving it as a follow-up.
        to: SessionId,
        /// The message.
        text: String,
    },

    /// Saved commands and prompts: a project's own and the global ones, or
    /// only the global ones when `project` is absent. By name.
    ListQuickCommands {
        /// Also this project's own commands, when set.
        #[serde(default)]
        project: Option<ProjectName>,
    },
    /// Save a quick command: new without an `id`, a replacement with one.
    SaveQuickCommand {
        /// The command to replace; absent creates a new one.
        #[serde(default)]
        id: Option<String>,
        /// The project it belongs to; absent offers it in every project.
        #[serde(default)]
        project: Option<ProjectName>,
        /// What the menu and the palette call it.
        name: String,
        /// Whether it is a shell command or a prompt.
        kind: crate::model::QuickCommandKind,
        /// The shell command line, or the prompt text.
        body: String,
    },
    /// Forget a quick command.
    RemoveQuickCommand {
        /// The command to forget.
        id: String,
    },
    /// Run a shell quick command in a new terminal in the workspace, which
    /// stays open on a shell afterwards so its output can be read. A prompt
    /// command is refused: sending a prompt is the conversation's to do,
    /// through `SendMessage` or `StartSession`.
    RunQuickCommand {
        /// The workspace whose dock the terminal opens in.
        workspace: WorkspaceId,
        /// The shell quick command to run.
        id: String,
        /// Terminal height in character cells.
        rows: u16,
        /// Terminal width in character cells.
        cols: u16,
    },

    /// The files in a workspace, best matches for `query` first.
    ///
    /// What `@` in the composer reaches for, and what a quick-open will.
    WorkspaceFiles {
        /// The workspace to list.
        workspace: WorkspaceId,
        /// Fuzzy filter on the path; every file when absent.
        query: Option<String>,
        /// Maximum entries; 30 when absent.
        limit: Option<u32>,
    },
    /// The commands a workspace offers after `/`.
    SlashCommands {
        /// The workspace whose commands to list.
        workspace: WorkspaceId,
        /// Filter on the name, typed after the slash.
        query: Option<String>,
    },
    /// Run `zg index` for a workspace in a daemon terminal, so agents started
    /// there are handed zvec-grep's search (`ginka-core::tools`). Answers
    /// the terminal; refused when `zg` is not installed.
    IndexWorkspace {
        /// The workspace to index.
        workspace: WorkspaceId,
        /// Terminal height in character cells.
        rows: u16,
        /// Terminal width in character cells.
        cols: u16,
    },
    /// Write a commit message for a workspace's changes on a cheap model
    /// (`docs/roadmap.md` §3.3 N9). Answers `Ack` at once; the message
    /// arrives as `DaemonEvent::CommitMessageGenerated`. `agent` names the
    /// driver to run it on; the workspace's latest session's otherwise, and
    /// `claude` failing that. `staged` describes only what is staged.
    GenerateCommitMessage {
        /// The workspace whose changes are described.
        workspace: WorkspaceId,
        /// Driver id to write it with.
        #[serde(default)]
        agent: Option<String>,
        /// Describe only what is staged.
        #[serde(default)]
        staged: bool,
    },
    /// Have an agent write a pull request's title and description from the
    /// branch's commits and its diff against the project's default branch,
    /// then push and open it with them (Orca's AI pull request details).
    /// Answers `Ack` at once; the result arrives as
    /// `DaemonEvent::PullRequestOpened`. `agent` is chosen as for
    /// `GenerateCommitMessage`.
    CreateGeneratedPullRequest {
        /// The workspace whose branch the pull request is from.
        workspace: WorkspaceId,
        /// Open it as a draft.
        #[serde(default)]
        draft: bool,
        /// Driver id to write the title and description with.
        #[serde(default)]
        agent: Option<String>,
    },
    /// The local branches of a workspace's repository, with which is checked
    /// out here and which are held by other worktrees.
    ListBranches {
        /// The workspace whose repository's branches to list.
        workspace: WorkspaceId,
    },
    /// Check a branch out inside a workspace, creating it from HEAD with
    /// `create`. The workspace keeps its name: its id never follows the branch
    /// (`AGENTS.md` rule 4), so nothing is re-keyed.
    CheckoutBranch {
        /// The workspace to switch.
        workspace: WorkspaceId,
        /// Short branch name to check out.
        branch: String,
        /// Create the branch from HEAD first.
        #[serde(default)]
        create: bool,
    },
    /// The skills the agents can load, across every ecosystem's roots and
    /// every registered project's — or one project's, when named
    /// (`docs/roadmap.md` §3.3 N11).
    ListSkills {
        /// Scan only this project's roots, not every registered project's.
        #[serde(default)]
        project: Option<ProjectName>,
    },
    /// Enable or disable every copy of a skill, by renaming its `SKILL.md`.
    /// Nothing is deleted; `project` narrows the search the same way.
    SetSkillEnabled {
        /// The skill's name, as listed.
        name: String,
        /// `true` enables every copy, `false` disables every copy.
        enabled: bool,
        /// Scan only this project's roots, not every registered project's.
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
    ComposerDraft {
        /// The workspace whose draft to read.
        workspace: WorkspaceId,
    },
    /// Keep what they are typing. An empty draft forgets it.
    SaveComposerDraft {
        /// The workspace the draft belongs to.
        workspace: WorkspaceId,
        /// The composer's text; empty forgets the draft.
        text: String,
    },

    /// What the work has cost, by day, by agent and by account, with the
    /// latest reading of every account's rate-limit windows.
    Usage {
        /// How many days back to count; 30 when absent.
        days: Option<u32>,
    },

    /// A changed image as it was and as it is under `source` (Orca's image
    /// diff). Each side is sent only when it is a recognised image within
    /// the preview limit. Every source is covered: a turn's two snapshots,
    /// a checkpoint or a branch's fork point against the worktree.
    ImageDiff {
        /// The workspace the image is in.
        workspace: WorkspaceId,
        /// What the old side is read from.
        source: crate::model::ChangeSource,
        /// Relative to the worktree root.
        path: String,
        /// Where the image was before a rename, when it moved.
        #[serde(default)]
        old_path: Option<String>,
    },
    /// The MCP servers the agent CLIs are configured with outside Ginka —
    /// the user's, and with `workspace` that repository's and that
    /// project's own (MonoCode's Settings → MCP). Read-only.
    ListMcpServers {
        /// Also this workspace's repository and project configuration, when set.
        #[serde(default)]
        workspace: Option<WorkspaceId>,
    },
    /// Add an MCP server to an agent CLI's own configuration, through that
    /// CLI (`claude mcp add`, `codex mcp add`). A project or local scope
    /// needs `workspace`, whose project it is written for. Not offered over
    /// MCP: an agent must not be able to install itself a server.
    AddMcpServer {
        /// The workspace whose project a project or local scope is written for.
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        /// What to add, where and for which CLI.
        spec: crate::model::McpServerSpec,
    },
    /// Remove an MCP server from an agent CLI's configuration, through that
    /// CLI. `scope` narrows where; `workspace` is needed for a project or
    /// local one.
    RemoveMcpServer {
        /// The workspace whose project a project or local scope refers to.
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        /// Whose configuration to change: `claude`, `codex`.
        provider: String,
        /// The server's configured name.
        name: String,
        /// Where to remove it from. When absent no scope is passed to the
        /// vendor's CLI, which picks its own, and the command runs from the
        /// home directory as for user scope.
        #[serde(default)]
        scope: Option<crate::model::McpScope>,
    },
    /// The checks of the pull request open from a workspace's branch, read
    /// with `gh pr checks` (Orca's checks view).
    PullRequestChecks {
        /// The workspace whose branch's pull request to read.
        workspace: WorkspaceId,
    },
    /// Hand the failing checks of the workspace's pull request to an agent
    /// to fix — Orca's "Fix broken checks". Routed like `ResolveConflicts`;
    /// refused when nothing failed. Answers with the conversation.
    FixFailingChecks {
        /// The workspace whose pull request failed.
        workspace: WorkspaceId,
        /// Which agent fixes them; chosen as for [`Request::ResolveConflicts`].
        #[serde(default)]
        agent: Option<String>,
    },

    /// Hand a worktree's conflicts — a merge, rebase or cherry-pick that
    /// stopped on them — to an agent to resolve and finish: Orca's "Resolve
    /// with AI". Refused when nothing is conflicted. Answers with the
    /// conversation the request went to.
    ResolveConflicts {
        /// The worktree with the conflicts.
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
        /// The workspace where the commit was refused.
        workspace: WorkspaceId,
        /// The commit message that was refused.
        message: String,
        /// What git and its hooks printed.
        output: String,
        /// Which agent fixes it; chosen as for [`Request::ResolveConflicts`].
        #[serde(default)]
        agent: Option<String>,
    },
    /// Leave a comment on a line of the diff.
    ///
    /// Orca's loop: marking the three places that are wrong is a better way to
    /// tell an agent what to fix than re-prompting it from scratch.
    ///
    /// `end_line` makes it cover `line..=end_line` (Orca's multi-line
    /// comments); refused when it comes before `line` or has none to start
    /// from.
    AddReviewComment {
        /// The workspace whose diff is commented on.
        workspace: WorkspaceId,
        /// Relative to the worktree root.
        path: String,
        /// One-based line on `side`; `None` comments on the whole file.
        line: Option<u32>,
        /// Last line of a multi-line comment, inclusive.
        #[serde(default)]
        end_line: Option<u32>,
        /// Which side of the diff the line numbers count on.
        side: DiffSide,
        /// What to tell the agent.
        text: String,
    },
    /// Every comment waiting in a workspace, in reading order.
    ListReviewComments {
        /// The workspace whose comments to list.
        workspace: WorkspaceId,
    },
    /// Take one comment back.
    RemoveReviewComment {
        /// The [`ReviewComment::id`] to remove.
        comment: String,
    },
    /// Send the batch to the agent as one message, and clear it.
    ///
    /// One message rather than one per comment: an agent given them together
    /// can see that three of them are the same mistake, and a turn per comment
    /// is three times the context and three times the cost.
    SendReviewComments {
        /// The workspace whose waiting comments are sent.
        workspace: WorkspaceId,
        /// The conversation to send them to. Its agent gets the batch as a
        /// follow-up, queued if it is still working.
        session: SessionId,
    },

    /// Every checkpoint taken in a workspace, newest first.
    ListCheckpoints {
        /// The workspace whose checkpoints to list.
        workspace: WorkspaceId,
    },
    /// Store a file the user attached, so a message can refer to it.
    ///
    /// The bytes travel base64-encoded because the wire is JSON; the daemon
    /// writes them once and every later mention is the reference it answers
    /// with (`docs/roadmap.md` §3.3 N6).
    UploadAttachment {
        /// The file's name as the user knows it; display only.
        name: String,
        /// The file's bytes, base64-encoded.
        data_base64: String,
    },
    /// Read a daemon-owned attachment as a bounded, signature-checked image.
    /// Returns no image for missing, unsafe, oversized or non-image references.
    ReadAttachmentImage {
        /// The `ginka-attachment:<id>` reference to read.
        reference: String,
    },
    /// Put the worktree back to a checkpoint's state.
    RestoreCheckpoint {
        /// The checkpoint whose tree is restored.
        checkpoint: CheckpointId,
    },

    /// Start a shell in a workspace.
    ///
    /// The daemon owns the pty, so the shell outlives the window that opened
    /// it — which is the whole point of the process split.
    OpenTerminal {
        /// The workspace the shell starts in.
        workspace: WorkspaceId,
        /// Terminal height in character cells.
        rows: u16,
        /// Terminal width in character cells.
        cols: u16,
    },
    /// The lines in a workspace's files that contain `query`.
    ///
    /// The other half of finding something: a reader who knows what the code
    /// says and not what it is called cannot get there by path.
    SearchContent {
        /// The workspace to search.
        workspace: WorkspaceId,
        /// Literal text to look for.
        query: String,
        /// Maximum matches; 30 when absent.
        limit: Option<u32>,
    },
    /// Find fuzzy file paths and literal source lines across a project's active workspaces.
    ///
    /// `limit` is shared across the complete result, rather than applied once
    /// per worktree, so one request remains bounded as projects grow.
    SearchProject {
        /// The project whose active workspaces are searched.
        project: ProjectName,
        /// Fuzzy path query and literal content text.
        query: String,
        /// Maximum hits of each kind, shared across every worktree.
        limit: Option<u32>,
    },

    /// Read one of a workspace's files, for the panel that shows it.
    ///
    /// Bounded and resolved inside the worktree by the daemon: a client asking
    /// for `../../.ssh/id_rsa` is asking the daemon to do something it will
    /// not do.
    ReadFile {
        /// The workspace whose worktree holds the file.
        workspace: WorkspaceId,
        /// Relative to the worktree root.
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
    WorkspaceTerminals {
        /// The workspace whose shells to list.
        workspace: WorkspaceId,
    },
    /// What a terminal has printed lately, for a window reattaching to it.
    TerminalHistory {
        /// The terminal to read back.
        terminal: TerminalId,
    },
    /// Send keystrokes to a terminal.
    WriteTerminal {
        /// The terminal to type into.
        terminal: TerminalId,
        /// Bytes to write to the pty, escapes included.
        data: String,
    },
    /// Tell a terminal how big its window is now.
    ResizeTerminal {
        /// The terminal to resize.
        terminal: TerminalId,
        /// New height in character cells.
        rows: u16,
        /// New width in character cells.
        cols: u16,
    },
    /// Close a terminal and stop its shell.
    CloseTerminal {
        /// The terminal to close.
        terminal: TerminalId,
    },

    /// Scheduled jobs: a project's, or every project's.
    ListCronJobs {
        /// Only this project's jobs, when set.
        project: Option<ProjectName>,
    },
    /// Save a scheduled job: new without an `id`, a replacement with one.
    /// The cron expression or `@once` timestamp is checked here, so a job
    /// that can never fire is refused when written rather than found silent.
    SaveCronJob {
        /// The job to replace; absent creates a new one.
        id: Option<i64>,
        /// The project it runs for.
        project: ProjectName,
        /// The workspace it runs in; the project's own checkout when absent.
        workspace: Option<WorkspaceId>,
        /// A chat job's conversation to continue instead of starting one
        /// (a reminder). Must be a conversation in `project`.
        #[serde(default)]
        session: Option<SessionId>,
        /// What lists call it.
        name: String,
        /// Five cron fields, an `@daily`-style macro, or `@once <RFC3339 timestamp>`.
        schedule: String,
        /// Whether the body is a prompt or a shell command.
        via: crate::model::CronVia,
        /// The driver a chat job starts; absent for a terminal job.
        agent: Option<String>,
        /// The prompt, or the command line.
        body: String,
        /// Run first on every scheduled firing; a failure skips the firing.
        #[serde(default)]
        precheck: Option<String>,
        /// Whether the schedule fires it.
        enabled: bool,
    },
    /// Forget a scheduled job and its history.
    RemoveCronJob {
        /// The job to forget.
        id: i64,
    },
    /// Fire a job now, as its schedule would — including skipping it while
    /// its previous run is still going. Its precheck is not run: asking for
    /// a run now is the answer the probe would have given.
    RunCronJob {
        /// The job to fire.
        id: i64,
    },
    /// A job's firings, most recent first.
    CronRuns {
        /// The job whose firings to list.
        id: i64,
        /// Maximum firings; 50 when absent, at most 500.
        limit: Option<u32>,
    },

    /// The last `limit` transcript entries before position `before` — or
    /// before the end — oldest first. How a window opens a long session on
    /// its latest page and reads the earlier ones on request.
    SessionTranscriptTail {
        /// The conversation to read.
        session: SessionId,
        /// Exclusive upper bound on `seq`; the end of the transcript when absent.
        before: Option<u64>,
        /// Maximum entries, capped by the daemon at 5000.
        limit: u32,
    },

    /// The daemon's settings (`settings.json`) as JSON, with every
    /// environment value replaced by `[set]`: the names are the reader's to
    /// see, the values — keys and tokens — are not the wire's to carry.
    DaemonSettings,
    /// Change one top-level setting to a JSON value, checked against the
    /// settings' shape before it is written and taken at once.
    UpdateDaemonSettings {
        /// The top-level key in `settings.json`.
        key: String,
        /// The new value, as JSON text.
        value: String,
    },
    /// The providers shipped by this build and their daemon-owned settings.
    ListProviderSettings,
    /// Change one provider without replacing other providers' settings.
    UpdateProviderSettings {
        /// The provider to change.
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
        /// The workspace whose browser loaded it.
        workspace: WorkspaceId,
        /// The address loaded, before it is cleaned.
        url: String,
        /// The page title, when it had one.
        title: Option<String>,
    },
    /// The pages to offer for what is typed in a workspace's address bar,
    /// most frecent first.
    BrowserSuggestions {
        /// The workspace whose history to search.
        workspace: WorkspaceId,
        /// What has been typed so far.
        query: String,
        /// Maximum pages; 8 when absent, at most 50.
        limit: Option<u32>,
    },
    /// Put the skills that teach an agent to drive Ginka — `ginka-start`,
    /// `ginka-chat`, `ginka-terminal`, `ginka-loop` — into Claude Code's and
    /// Codex's skills directories on the daemon's host. A different file
    /// already there is left alone unless `force`.
    InstallBundledSkills {
        /// Overwrite a different file already in place.
        force: bool,
    },

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
    /// Provider model id; the provider's default when absent.
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
    /// Answers [`Request::ListProjects`].
    Projects {
        /// Every registered project, in sidebar order.
        projects: Vec<Project>,
    },
    /// Answers [`Request::AddProject`].
    Project {
        /// The project as registered.
        project: Project,
    },
    /// Answers [`Request::ListWorkspaces`].
    Workspaces {
        /// Every matching workspace, reconciled against git.
        workspaces: Vec<WorkspaceSummary>,
    },
    /// Answers [`Request::ListWorkspaceFolders`], ordered by exact label.
    WorkspaceFolders {
        /// Nonempty folders, each with active and archived member counts.
        folders: Vec<WorkspaceFolder>,
    },
    /// A workspace that was just created, by [`Request::CreateWorkspace`] or
    /// [`Request::CreateScratchWorkspace`].
    Workspace {
        /// The new workspace.
        workspace: WorkspaceSummary,
    },
    /// Answers [`Request::ListAgents`].
    Agents {
        /// One row per known driver, whether installed or not.
        agents: Vec<AgentStatus>,
    },
    /// Answers [`Request::Accounts`].
    Accounts {
        /// Every login of every provider, the defaults first.
        accounts: Vec<Account>,
    },
    /// Answers [`Request::AddAccount`].
    Account {
        /// The account as created.
        account: Account,
    },
    /// A reading of an account's windows, or none when the provider could
    /// not be asked.
    PlanUsage {
        /// The reading; `None` when the provider could not be asked.
        snapshot: Option<PlanSnapshot>,
    },
    /// Answers [`Request::ListSessions`].
    Sessions {
        /// Matching sessions, newest first.
        sessions: Vec<Session>,
    },
    /// Answers [`Request::SearchSessions`].
    SessionMatches {
        /// Where the query was found, one row per transcript entry.
        matches: Vec<SessionMatch>,
    },
    /// A session that was just started, forked, adopted or handed work.
    Session {
        /// The session's full record.
        session: Session,
    },
    /// Provider options were applied to this session. When the outcome says a
    /// restart was required, `session` is the context-preserving replacement.
    SessionOptionsApplied {
        /// The session that runs later turns: the same one, or its replacement.
        session: Session,
        /// Whether the change was absorbed or forced a restart.
        outcome: crate::provider::OptionOutcome,
    },
    /// Answers [`Request::SessionTranscript`] and [`Request::SessionTranscriptTail`].
    Transcript {
        /// Entries in ascending `seq` order.
        entries: Vec<TranscriptEntry>,
    },
    /// Answers [`Request::QueuedMessages`].
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
    /// Answers [`Request::ListCheckpoints`].
    Checkpoints {
        /// Every checkpoint in the workspace, newest first.
        checkpoints: Vec<Checkpoint>,
    },
    /// The two sides of a changed image; `None` where there is no image.
    ImageDiff {
        /// The image as it was under the source.
        before: Option<crate::model::FileImage>,
        /// The image as it is now.
        after: Option<crate::model::FileImage>,
    },
    /// MCP servers configured outside Ginka, by provider and scope.
    McpServers {
        /// One row per server, provider and scope.
        servers: Vec<crate::model::McpServerEntry>,
    },
    /// A pull request's checks, in the order `gh` lists them.
    Checks {
        /// One row per check.
        checks: Vec<crate::model::CheckRun>,
    },
    /// Answers [`Request::WorkspaceChanges`].
    Changes {
        /// Every changed file, with what it was measured against.
        changes: Changes,
    },
    /// Recent commits in newest-first order.
    History {
        /// Newest first, bounded by the request's limit.
        commits: Vec<GitCommit>,
    },
    /// Answers [`Request::WorkspaceFiles`].
    Files {
        /// Best matches first.
        files: Vec<FileEntry>,
    },
    /// Answers [`Request::SlashCommands`].
    Commands {
        /// The commands that match what was typed.
        commands: Vec<SlashCommand>,
    },
    /// Answers [`Request::ListBranches`].
    Branches {
        /// Every local branch of the workspace's repository.
        branches: Vec<BranchInfo>,
    },
    /// Answers [`Request::ListSkills`].
    Skills {
        /// One entry per skill name, carrying every copy.
        skills: Vec<Skill>,
        /// The scan stopped at its cap; the list is what fitted.
        truncated: bool,
    },
    /// Answers [`Request::ListReviewComments`].
    ReviewComments {
        /// The waiting comments, in reading order.
        comments: Vec<ReviewComment>,
    },
    /// Answers [`Request::UploadAttachment`].
    Attachment {
        /// The stored file and the reference a message uses for it.
        attachment: Attachment,
    },
    /// A previewable attachment, or no image if it cannot be safely displayed.
    AttachmentImage {
        /// The image; `None` when the reference is missing, unsafe, oversized or not an image.
        image: Option<crate::model::FileImage>,
    },
    /// Answers [`Request::ComposerDraft`].
    Draft {
        /// The saved draft; empty when there is none.
        text: String,
    },
    /// A terminal that was just opened, for a shell, a sign-in, an index or a quick command.
    Terminal {
        /// The new terminal; its output arrives as `DaemonEvent::TerminalOutput`.
        terminal: TerminalId,
    },
    /// Answers [`Request::ReadFile`] and [`Request::WriteFile`].
    FileContent {
        /// The file as read, or as written with its new revision.
        file: FileContent,
    },
    /// Answers [`Request::SearchContent`].
    Matches {
        /// Matching lines, bounded by the request's limit.
        matches: Vec<ContentMatch>,
    },
    /// Path and content hits from more than one worktree, tagged with their source.
    WorkspaceMatches {
        /// Fuzzy path hits, under their own shared project-wide limit.
        files: Vec<WorkspaceFileMatch>,
        /// Literal content hits, under their own shared project-wide limit.
        matches: Vec<WorkspaceContentMatch>,
    },
    /// Answers [`Request::WorkspaceTerminals`].
    Terminals {
        /// The shells still running in the workspace.
        terminals: Vec<TerminalInfo>,
    },
    /// What a fan-out started, and what it could not.
    FannedOut {
        /// The sessions that started, in the order their attempts were asked for.
        started: Vec<Session>,
        /// One line per attempt that did not start, in the order they were
        /// asked for: an arm that failed must not take the others with it.
        failed: Vec<String>,
    },
    /// What a terminal printed before this window was looking.
    TerminalHistory {
        /// The retained output, escapes and all, oldest first.
        data: String,
    },
    /// Answers [`Request::Usage`].
    Usage {
        /// One row per day in the requested range.
        by_day: Vec<UsageRow>,
        /// One row per driver.
        by_agent: Vec<UsageRow>,
        /// One row per account.
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
    /// What a merge did.
    Merged {
        /// Which branch moved, where to, and whether it fast-forwarded.
        outcome: crate::model::MergeOutcome,
    },
    /// Answers [`Request::Commit`].
    Committed {
        /// The new commit's full object id.
        commit: String,
    },
    /// Answers [`Request::ListConnectors`].
    Connectors {
        /// One entry per connector this daemon hosts.
        connectors: Vec<ConnectorState>,
    },
    /// A pull request exists for the branch, and this is where.
    PullRequest {
        /// The pull request's web address.
        url: String,
    },
    /// The conversations [`Request::CliSessions`] found.
    CliSessions {
        /// Newest first, without the ones Ginka's sessions already hold.
        sessions: Vec<crate::model::CliSession>,
    },
    /// What [`Request::Sync`] did.
    Synced {
        /// Commits came in from the remote.
        pulled: bool,
        /// Commits went out, or the branch was published.
        pushed: bool,
    },
    /// Answers [`Request::ListNotes`].
    Notes {
        /// Matching notes, most recently touched first.
        notes: Vec<Note>,
    },
    /// Answers [`Request::SaveNote`].
    Note {
        /// The note as stored.
        note: Note,
    },
    /// The daemon's settings, secrets left out.
    DaemonSettings {
        /// The settings file as JSON text, every environment value replaced by `[set]`.
        json: String,
    },
    /// Runtime provider choices, including disabled providers.
    ProviderSettings {
        /// One row per provider this build ships.
        providers: Vec<ProviderSetting>,
    },
    /// Pages the address bar can offer.
    BrowserSuggestions {
        /// Most frecent first.
        pages: Vec<crate::model::VisitedPage>,
    },
    /// What installing Ginka's own skills did, a row per skill and place.
    BundledSkillsInstalled {
        /// One row per skill and destination.
        results: Vec<crate::model::BundledSkillInstall>,
    },
    /// Answers [`Request::ListCronJobs`].
    CronJobs {
        /// The matching jobs.
        jobs: Vec<crate::model::CronJob>,
    },
    /// Answers [`Request::SaveCronJob`] and [`Request::RunCronJob`].
    CronJob {
        /// The job as stored, with its next and last run.
        job: crate::model::CronJob,
    },
    /// Answers [`Request::CronRuns`].
    CronRuns {
        /// The job's firings, most recent first.
        runs: Vec<crate::model::CronRun>,
    },
    /// Answers [`Request::ListTickets`].
    Tickets {
        /// Matching tickets, newest first.
        tickets: Vec<crate::model::Ticket>,
    },
    /// A ticket that was just raised or changed.
    Ticket {
        /// The ticket as stored.
        ticket: crate::model::Ticket,
    },
    /// Answers [`Request::ListQuickCommands`].
    QuickCommands {
        /// Matching commands, by name.
        commands: Vec<crate::model::QuickCommand>,
    },
    /// Answers [`Request::SaveQuickCommand`].
    QuickCommand {
        /// The command as stored.
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
            Request::ListWorkspaceFolders {
                project: ProjectName("comet".into()),
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
