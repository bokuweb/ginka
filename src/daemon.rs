//! The app's connection to the daemon.
//!
//! The window holds view state and nothing authoritative (`AGENTS.md` rule 1),
//! so everything it draws comes from here: it asks the daemon, and the daemon
//! owns the database, the git operations and the agent processes. Closing the
//! window stops nothing.
//!
//! The connection is made lazily and kept. A daemon that has gone — restarted,
//! stopped, upgraded — is noticed on the next request, and the one after it
//! starts a new one, because a UI that has to be relaunched to reconnect is a
//! UI that gets relaunched.

use ginka_client::{Client, Discovery, Event};
use ginka_core::Paths;
use std::path::PathBuf;

use ginka_protocol::model::{
    Account, AgentStatus, Attachment, BranchInfo, ChangeSource, Changes, Checkpoint, ContentMatch,
    FileContent, FileEntry, Note, PlanSnapshot, Project, ReviewComment, Session, SessionMatch,
    Skill, SlashCommand, TerminalInfo, TranscriptEntry, WorkspaceSummary,
};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{AccountId, CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
use ginka_ui::workspace::SessionRow;
use std::sync::{Arc, Mutex};

/// A daemon connection shared by everything in the window.
pub struct DaemonLink {
    discovery: Discovery,
    /// Whether daemon-host paths also name files on the window's machine.
    local_paths: bool,
    /// `None` until the first request, and again after one fails.
    client: Mutex<Option<Arc<Client>>>,
}

/// Everything the composer chooses when it starts a new conversation.
pub(crate) struct SessionLaunch {
    /// Workspace in which the agent runs.
    pub(crate) workspace: WorkspaceId,
    /// Driver selected for the conversation.
    pub(crate) agent: String,
    /// First user message.
    pub(crate) prompt: String,
    /// Optional provider model override.
    pub(crate) model: Option<String>,
    /// Optional provider reasoning-effort override.
    pub(crate) reasoning_effort: Option<String>,
    /// Optional provider service-tier override.
    pub(crate) service_tier: Option<String>,
    /// Access policy fixed for the conversation.
    pub(crate) access: Option<ginka_protocol::AccessMode>,
    /// Optional provider login selected for the conversation.
    pub(crate) account: Option<AccountId>,
}

impl DaemonLink {
    /// Build a link to the daemon that owns `paths`.
    pub fn new(paths: &Paths) -> Arc<Self> {
        let discovery = Discovery::new(paths.daemon_handshake()).with_home(paths.root());
        Arc::new(Self {
            local_paths: discovery.location().allows_local_paths(),
            discovery,
            client: Mutex::new(None),
        })
    }

    /// Whether a daemon-host path may be passed to a local picker or process.
    pub fn allows_local_paths(&self) -> bool {
        self.local_paths
    }

    /// The rows the sidebar draws.
    ///
    /// A daemon that cannot be reached yields an empty list rather than an
    /// error: the window opening empty and saying so is a better failure than
    /// the window not opening.
    pub async fn workspaces(&self, now: i64) -> Vec<SessionRow> {
        match self.ask(Request::ListWorkspaces { project: None }).await {
            Some(Response::Workspaces { workspaces }) => workspaces
                .iter()
                .map(|summary| SessionRow::from_summary(summary, now))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Every registered project, in the order the sidebar lists them.
    ///
    /// Asked for separately from the workspaces because a project with no
    /// worktree yet is still a heading the reader can start a chat under, and
    /// a list assembled from workspaces could not show it.
    pub async fn projects(&self) -> Vec<Project> {
        match self.ask(Request::ListProjects).await {
            Some(Response::Projects { projects }) => projects,
            _ => Vec::new(),
        }
    }

    /// Make somewhere to work with no project at all.
    ///
    /// What "work without a project" means: the daemon creates a dated
    /// directory under its own state and registers it, so a question that
    /// needs somewhere to run does not need a repository first.
    pub async fn create_scratch(&self) -> Option<WorkspaceSummary> {
        match self
            .ask(Request::CreateScratchWorkspace { name: None })
            .await
        {
            Some(Response::Workspace { workspace }) => Some(workspace),
            _ => None,
        }
    }

    /// What each agent CLI on this machine says about itself.
    ///
    /// The daemon caches the probe, so asking on every tick costs a request
    /// rather than two subprocesses per agent.
    pub async fn agents(&self) -> Vec<AgentStatus> {
        match self.ask(Request::ListAgents).await {
            Some(Response::Agents { agents }) => agents,
            _ => Vec::new(),
        }
    }

    /// The next thing the daemon announced.
    ///
    /// `None` means the stream ended — the daemon stopped, or the connection
    /// dropped — and the caller should wait before asking again rather than
    /// spinning on a socket that is not there.
    pub async fn next_event(&self) -> Option<Event> {
        let client = self.client().await?;
        match client.next_event().await {
            Some(event) => Some(event),
            None => {
                self.forget();
                None
            }
        }
    }

    /// A page of a session's transcript, from after `after`.
    ///
    /// The centre column asks for what it has not folded in yet, so opening a
    /// long session reads it once and every later tick reads only the tail.
    pub async fn transcript(&self, session: &SessionId, after: u64) -> Vec<TranscriptEntry> {
        match self
            .ask(Request::SessionTranscript {
                session: session.clone(),
                after: (after > 0).then_some(after),
                limit: None,
            })
            .await
        {
            Some(Response::Transcript { entries }) => entries,
            _ => Vec::new(),
        }
    }

    /// Find stored transcript entries in one workspace.
    pub async fn search_sessions(
        &self,
        workspace: WorkspaceId,
        query: String,
    ) -> Result<Vec<SessionMatch>, String> {
        match self
            .ask_result(Request::SearchSessions {
                workspace: Some(workspace),
                query,
                limit: Some(100),
            })
            .await?
        {
            Response::SessionMatches { matches } => Ok(matches),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }

    /// Upload bytes selected by this client to the daemon-owned attachment store.
    pub async fn upload_attachment(
        &self,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<Attachment, String> {
        use base64::Engine as _;

        match self
            .ask_result(Request::UploadAttachment {
                name,
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
            .await?
        {
            Response::Attachment { attachment } => Ok(attachment),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }

    /// Start an agent in a workspace, and return the session it created.
    pub async fn start_session(&self, launch: SessionLaunch) -> Option<Session> {
        match self
            .ask(Request::StartSession {
                workspace: launch.workspace,
                agent: launch.agent,
                prompt: launch.prompt,
                model: launch.model,
                reasoning_effort: launch.reasoning_effort,
                service_tier: launch.service_tier,
                account: launch.account,
                access_mode: launch.access,
                origin: None,
            })
            .await
        {
            Some(Response::Session { session }) => Some(session),
            _ => None,
        }
    }

    /// Replace the provider options used by later turns of a conversation.
    pub async fn update_session_options(
        &self,
        session: SessionId,
        model: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    ) -> Option<(Session, ginka_protocol::OptionOutcome)> {
        match self
            .ask(Request::UpdateSessionOptions {
                session,
                model,
                reasoning_effort,
                service_tier,
            })
            .await
        {
            Some(Response::SessionOptionsApplied { session, outcome }) => Some((session, outcome)),
            _ => None,
        }
    }

    /// What the work cost over `days`, and where each login stands.
    pub async fn usage(&self, days: u32) -> Option<ginka_ui::reports::UsageReport> {
        match self.ask(Request::Usage { days: Some(days) }).await {
            Some(Response::Usage {
                by_day,
                by_agent,
                by_account,
                plans,
                rates_fetched_at,
            }) => Some(ginka_ui::reports::UsageReport {
                by_day,
                by_agent,
                by_account,
                plans,
                rates_fetched_at,
            }),
            _ => None,
        }
    }

    /// The agents' own reusable skills, narrowed to one project when asked.
    pub async fn skills(&self, project: Option<ProjectName>) -> Result<(Vec<Skill>, bool), String> {
        match self.ask_result(Request::ListSkills { project }).await? {
            Response::Skills { skills, truncated } => Ok((skills, truncated)),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }

    /// Enable or disable every installed copy of a named skill.
    pub async fn set_skill_enabled(
        &self,
        name: String,
        enabled: bool,
        project: Option<ProjectName>,
    ) -> Result<(), String> {
        match self
            .ask_result(Request::SetSkillEnabled {
                name,
                enabled,
                project,
            })
            .await?
        {
            Response::Ack => Ok(()),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }

    /// Add a login for a provider: a directory for its CLI to sign into.
    ///
    /// The daemon's refusal — a bad id, a name already taken — comes back as
    /// the sentence it gave, because the dialog shows it to the reader.
    pub async fn add_account(
        &self,
        id: AccountId,
        provider: ginka_protocol::ProviderKind,
        label: String,
    ) -> Result<Account, String> {
        match self
            .ask_result(Request::AddAccount {
                id,
                provider,
                label,
            })
            .await?
        {
            Response::Account { account } => Ok(account),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }

    /// Every login of every provider, with what the daemon last learned
    /// about each being signed in.
    pub async fn accounts(&self) -> Vec<Account> {
        match self.ask(Request::Accounts).await {
            Some(Response::Accounts { accounts }) => accounts,
            _ => Vec::new(),
        }
    }

    /// Select the login future sessions of its provider use.
    pub async fn select_account(&self, account: &AccountId) -> Result<(), String> {
        self.git_sync(Request::SelectAccount {
            id: account.clone(),
        })
        .await
    }

    /// The latest reading of every account's rate-limit windows.
    ///
    /// Comes with the usage report, of which the readings are the part the
    /// composer wants; the totals are asked for a day so the rest stays
    /// small.
    pub async fn plans(&self) -> Vec<PlanSnapshot> {
        match self.ask(Request::Usage { days: Some(1) }).await {
            Some(Response::Usage { plans, .. }) => plans,
            _ => Vec::new(),
        }
    }

    /// Ask the provider for an account's windows now. The daemon pushes the
    /// reading too, so every window sees it.
    pub async fn refresh_plan(&self, account: &AccountId) -> Option<PlanSnapshot> {
        match self
            .ask(Request::RefreshPlanUsage {
                account: account.clone(),
            })
            .await
        {
            Some(Response::PlanUsage { snapshot }) => snapshot,
            _ => None,
        }
    }

    /// Run the vendor's own sign-in for an account in a terminal in the
    /// workspace's dock, and return the terminal.
    pub async fn login_account(
        &self,
        account: &AccountId,
        workspace: &WorkspaceId,
        rows: u16,
        cols: u16,
    ) -> Option<TerminalId> {
        match self
            .ask(Request::LoginAccount {
                id: account.clone(),
                workspace: workspace.clone(),
                rows,
                cols,
            })
            .await
        {
            Some(Response::Terminal { terminal }) => Some(terminal),
            _ => None,
        }
    }

    /// The checkpoints taken in a workspace, newest first.
    pub async fn checkpoints(&self, workspace: &WorkspaceId) -> Vec<Checkpoint> {
        match self
            .ask(Request::ListCheckpoints {
                workspace: workspace.clone(),
            })
            .await
        {
            Some(Response::Checkpoints { checkpoints }) => checkpoints,
            _ => Vec::new(),
        }
    }

    /// What has changed in a workspace, against `source`.
    pub async fn changes(&self, workspace: &WorkspaceId, source: ChangeSource) -> Option<Changes> {
        match self
            .ask(Request::WorkspaceChanges {
                workspace: workspace.clone(),
                source,
            })
            .await
        {
            Some(Response::Changes { changes }) => Some(changes),
            _ => None,
        }
    }

    /// Commit a workspace's work, everything in it.
    ///
    /// The error is the message git gave, because that is the one the user can
    /// act on: an identity that is not configured, a hook that refused, or
    /// nothing to commit at all.
    /// Have an agent write a commit message; it arrives later as
    /// `DaemonEvent::CommitMessageGenerated`.
    pub async fn generate_commit_message(
        &self,
        workspace: &WorkspaceId,
        only_staged: bool,
    ) -> Result<(), String> {
        let client = self.client().await.ok_or("no daemon")?;
        client
            .request(Request::GenerateCommitMessage {
                workspace: workspace.clone(),
                agent: None,
                staged: only_staged,
            })
            .await
            .map(|_| ())
            .map_err(|error| error.message)
    }

    pub async fn commit(
        &self,
        workspace: &WorkspaceId,
        message: String,
        all: bool,
    ) -> Result<(), String> {
        let client = self.client().await.ok_or("no daemon")?;
        match client
            .request(Request::Commit {
                workspace: workspace.clone(),
                message,
                all,
            })
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                if error.code == "failed" {
                    Err(error.message)
                } else {
                    self.forget();
                    Err(error.message)
                }
            }
        }
    }

    /// Merge a workspace's branch into the branch its project is on,
    /// committing what it left uncommitted with `message` first.
    pub async fn merge_workspace(
        &self,
        workspace: &WorkspaceId,
        message: String,
    ) -> Result<ginka_protocol::model::MergeOutcome, String> {
        match self
            .ask_result(Request::MergeWorkspace {
                workspace: workspace.clone(),
                into: None,
                message: Some(message),
            })
            .await?
        {
            Response::Merged { outcome } => Ok(outcome),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// Push a workspace branch through the daemon.
    pub async fn push(&self, workspace: &WorkspaceId) -> Result<(), String> {
        self.git_sync(Request::Push {
            workspace: workspace.clone(),
        })
        .await
    }

    /// Fetch and fast-forward a clean workspace branch through the daemon.
    pub async fn pull(&self, workspace: &WorkspaceId) -> Result<(), String> {
        self.git_sync(Request::Pull {
            workspace: workspace.clone(),
        })
        .await
    }

    async fn git_sync(&self, request: Request) -> Result<(), String> {
        let client = self.client().await.ok_or("no daemon")?;
        client
            .request(request)
            .await
            .map(|_| ())
            .map_err(|error| error.message)
    }

    /// Recent workspace commits, newest first and bounded by the daemon.
    pub async fn history(
        &self,
        workspace: &WorkspaceId,
        limit: u32,
    ) -> Vec<ginka_protocol::model::GitCommit> {
        match self
            .ask(Request::WorkspaceHistory {
                workspace: workspace.clone(),
                limit: Some(limit),
            })
            .await
        {
            Some(Response::History { commits }) => commits,
            _ => Vec::new(),
        }
    }

    /// Put a file into the next commit, or take it back out.
    /// Register a repository or folder.
    ///
    /// The path is one this window picked, so it is a path on *this* machine.
    /// That is only the same thing as a daemon-host path while the daemon was
    /// discovered or spawned locally, which is why the picker is offered only then
    /// (`docs/roadmap.md` §4.1).
    /// The registered project comes back so the caller can aim the next chat
    /// at what the reader just added, rather than at whatever was selected
    /// before they went looking for a folder.
    pub async fn add_project(&self, path: PathBuf, label: Option<String>) -> Option<Project> {
        match self.ask(Request::AddProject { path, label }).await {
            Some(Response::Project { project }) => Some(project),
            _ => None,
        }
    }

    pub async fn stage(&self, workspace: &WorkspaceId, path: &str, staged: bool) {
        self.ask(Request::StageFile {
            workspace: workspace.clone(),
            path: path.to_string(),
            staged,
        })
        .await;
    }

    /// Move one current diff hunk into or out of the index.
    pub async fn stage_hunk(
        &self,
        workspace: &WorkspaceId,
        path: &str,
        header: &str,
        staged: bool,
    ) -> Result<(), String> {
        self.git_sync(Request::StageHunk {
            workspace: workspace.clone(),
            path: path.to_string(),
            header: header.to_string(),
            staged,
        })
        .await
    }

    /// Permanently discard one current unstaged hunk.
    pub async fn revert_hunk(
        &self,
        workspace: &WorkspaceId,
        path: &str,
        header: &str,
    ) -> Result<(), String> {
        self.git_sync(Request::RevertHunk {
            workspace: workspace.clone(),
            path: path.to_string(),
            header: header.to_string(),
        })
        .await
    }

    /// Throw away a file's uncommitted work.
    pub async fn revert(&self, workspace: &WorkspaceId, path: &str) {
        self.ask(Request::RevertFile {
            workspace: workspace.clone(),
            path: path.to_string(),
        })
        .await;
    }

    /// Answer a question or a plan the agent is waiting on.
    pub async fn respond(
        &self,
        session: &SessionId,
        request_id: &str,
        response: &str,
    ) -> Result<(), String> {
        self.ask_result(Request::RespondToAgent {
            session: session.clone(),
            request_id: request_id.to_string(),
            response: response.to_string(),
        })
        .await
        .map(|_| ())
    }

    /// The lines in a workspace's files that contain `query`.
    pub async fn search_content(&self, workspace: &WorkspaceId, query: &str) -> Vec<ContentMatch> {
        match self
            .ask(Request::SearchContent {
                workspace: workspace.clone(),
                query: query.to_string(),
                limit: None,
            })
            .await
        {
            Some(Response::Matches { matches }) => matches,
            _ => Vec::new(),
        }
    }

    /// Literal source hits across every active workspace in a project.
    pub async fn search_project(
        &self,
        project: &ginka_protocol::ProjectName,
        query: &str,
    ) -> (
        Vec<ginka_protocol::model::WorkspaceFileMatch>,
        Vec<ginka_protocol::model::WorkspaceContentMatch>,
    ) {
        match self
            .ask(Request::SearchProject {
                project: project.clone(),
                query: query.to_string(),
                limit: None,
            })
            .await
        {
            Some(Response::WorkspaceMatches { files, matches }) => (files, matches),
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// One of a workspace's files, as text.
    pub async fn read_file(&self, workspace: &WorkspaceId, path: &str) -> Option<FileContent> {
        match self
            .ask(Request::ReadFile {
                workspace: workspace.clone(),
                path: path.to_string(),
            })
            .await
        {
            Some(Response::FileContent { file }) => Some(file),
            _ => None,
        }
    }

    /// Save a file only if it still has the revision the editor opened.
    pub async fn write_file(
        &self,
        workspace: &WorkspaceId,
        path: &str,
        text: String,
        expected_revision: String,
    ) -> Result<FileContent, String> {
        match self
            .ask_result(Request::WriteFile {
                workspace: workspace.clone(),
                path: path.to_string(),
                text,
                expected_revision,
            })
            .await?
        {
            Response::FileContent { file } => Ok(file),
            response => Err(format!("unexpected response: {response:?}")),
        }
    }

    /// The shells the daemon is running in a workspace.
    pub async fn terminals(&self, workspace: &WorkspaceId) -> Vec<TerminalInfo> {
        match self
            .ask(Request::WorkspaceTerminals {
                workspace: workspace.clone(),
            })
            .await
        {
            Some(Response::Terminals { terminals }) => terminals,
            _ => Vec::new(),
        }
    }

    /// What a shell printed while this window was not looking.
    pub async fn terminal_history(&self, terminal: &TerminalId) -> Option<String> {
        match self
            .ask(Request::TerminalHistory {
                terminal: terminal.clone(),
            })
            .await
        {
            Some(Response::TerminalHistory { data }) => Some(data),
            _ => None,
        }
    }

    /// Leave a comment on a line of the diff.
    pub async fn add_comment(
        &self,
        workspace: &WorkspaceId,
        path: &str,
        line: Option<u32>,
        text: String,
    ) {
        self.ask(Request::AddReviewComment {
            workspace: workspace.clone(),
            path: path.to_string(),
            line,
            side: ginka_protocol::DiffSide::New,
            text,
        })
        .await;
    }

    /// The comments waiting in a workspace, in reading order.
    pub async fn comments(&self, workspace: &WorkspaceId) -> Vec<ReviewComment> {
        match self
            .ask(Request::ListReviewComments {
                workspace: workspace.clone(),
            })
            .await
        {
            Some(Response::ReviewComments { comments }) => comments,
            _ => Vec::new(),
        }
    }

    /// Send the waiting comments to a session's agent as one message.
    pub async fn send_review(&self, workspace: &WorkspaceId, session: &SessionId) {
        self.ask(Request::SendReviewComments {
            workspace: workspace.clone(),
            session: session.clone(),
        })
        .await;
    }

    /// Put a workspace back to the state a checkpoint captured.
    pub async fn restore(&self, checkpoint: &CheckpointId) {
        self.ask(Request::RestoreCheckpoint {
            checkpoint: checkpoint.clone(),
        })
        .await;
    }

    /// The files in a workspace that match `query`, best first.
    pub async fn files(&self, workspace: &WorkspaceId, query: &str) -> Vec<FileEntry> {
        match self
            .ask(Request::WorkspaceFiles {
                workspace: workspace.clone(),
                query: Some(query.to_string()),
                limit: None,
            })
            .await
        {
            Some(Response::Files { files }) => files,
            _ => Vec::new(),
        }
    }

    /// The bounded catalogue used to build the workspace file tree.
    pub async fn file_tree(&self, workspace: &WorkspaceId) -> (Vec<FileEntry>, bool) {
        let limit = ginka_ui::file_tree::TREE_FILE_LIMIT;
        let files = match self
            .ask(Request::WorkspaceFiles {
                workspace: workspace.clone(),
                query: None,
                // Ask for one sentinel beyond the UI bound so truncation is
                // visible rather than silently presenting a complete tree.
                limit: Some((limit + 1) as u32),
            })
            .await
        {
            Some(Response::Files { files }) => files,
            _ => Vec::new(),
        };
        ginka_ui::file_tree::bounded_catalogue(files, limit)
    }

    /// The commands this workspace offers after `/`.
    pub async fn commands(&self, workspace: &WorkspaceId, query: &str) -> Vec<SlashCommand> {
        match self
            .ask(Request::SlashCommands {
                workspace: workspace.clone(),
                query: Some(query.to_string()),
            })
            .await
        {
            Some(Response::Commands { commands }) => commands,
            _ => Vec::new(),
        }
    }

    /// What was being typed in a workspace when it was last left.
    pub async fn draft(&self, workspace: &WorkspaceId) -> String {
        match self
            .ask(Request::ComposerDraft {
                workspace: workspace.clone(),
            })
            .await
        {
            Some(Response::Draft { text }) => text,
            _ => String::new(),
        }
    }

    /// Keep what is being typed, so leaving does not lose it.
    pub async fn save_draft(&self, workspace: &WorkspaceId, text: String) {
        self.ask(Request::SaveComposerDraft {
            workspace: workspace.clone(),
            text,
        })
        .await;
    }

    /// Start a shell in a workspace, sized for the dock as it is now.
    pub async fn open_terminal(
        &self,
        workspace: &WorkspaceId,
        rows: u16,
        cols: u16,
    ) -> Option<TerminalId> {
        match self
            .ask(Request::OpenTerminal {
                workspace: workspace.clone(),
                rows,
                cols,
            })
            .await
        {
            Some(Response::Terminal { terminal }) => Some(terminal),
            _ => None,
        }
    }

    /// Start zvec-grep indexing in a daemon-owned terminal.
    pub async fn index_workspace(
        &self,
        workspace: &WorkspaceId,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalId, String> {
        match self
            .ask_result(Request::IndexWorkspace {
                workspace: workspace.clone(),
                rows,
                cols,
            })
            .await?
        {
            Response::Terminal { terminal } => Ok(terminal),
            _ => Err("the daemon returned the wrong response for workspace indexing".into()),
        }
    }

    /// Send keystrokes to a terminal.
    pub async fn write_terminal(&self, terminal: &TerminalId, data: String) {
        self.ask(Request::WriteTerminal {
            terminal: terminal.clone(),
            data,
        })
        .await;
    }

    /// Tell a terminal how big its window is now.
    pub async fn resize_terminal(&self, terminal: &TerminalId, rows: u16, cols: u16) {
        self.ask(Request::ResizeTerminal {
            terminal: terminal.clone(),
            rows,
            cols,
        })
        .await;
    }

    /// Close a terminal and stop its shell.
    pub async fn close_terminal(&self, terminal: &TerminalId) {
        self.ask(Request::CloseTerminal {
            terminal: terminal.clone(),
        })
        .await;
    }

    /// Stop the agent working in a session.
    pub async fn cancel_session(&self, session: &SessionId) {
        self.ask(Request::CancelSession {
            session: session.clone(),
        })
        .await;
    }

    /// Send a follow-up to a running session.
    pub async fn send_message(&self, session: &SessionId, text: String) {
        self.ask(Request::SendMessage {
            session: session.clone(),
            text,
        })
        .await;
    }

    /// Follow-ups waiting behind this session's active turn.
    pub async fn queued_messages(
        &self,
        session: &SessionId,
    ) -> (Vec<ginka_protocol::model::QueuedMessage>, bool, bool) {
        match self
            .ask_result(Request::QueuedMessages {
                session: session.clone(),
            })
            .await
        {
            Ok(Response::QueuedMessages {
                messages,
                can_send_now,
                paused,
            }) => (messages, can_send_now, paused),
            _ => (Vec::new(), false, false),
        }
    }

    /// Ask one prompt in a worktree per attempt; answers with the sessions
    /// that started and a line for each that did not.
    pub async fn fan_out(
        &self,
        project: ProjectName,
        branch_prefix: String,
        prompt: String,
        attempts: Vec<ginka_protocol::rpc::Attempt>,
    ) -> Result<(Vec<Session>, Vec<String>), String> {
        match self
            .ask_result(Request::FanOut {
                project,
                branch_prefix,
                base: None,
                prompt,
                attempts,
            })
            .await?
        {
            Response::FannedOut { started, failed } => Ok((started, failed)),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// A project's saved commands and the global ones.
    pub async fn quick_commands(
        &self,
        project: Option<ProjectName>,
    ) -> Vec<ginka_protocol::model::QuickCommand> {
        match self.ask(Request::ListQuickCommands { project }).await {
            Some(Response::QuickCommands { commands }) => commands,
            _ => Vec::new(),
        }
    }

    /// Save a quick command; the refusal is the daemon's own words.
    pub async fn save_quick_command(
        &self,
        project: Option<ProjectName>,
        name: String,
        kind: ginka_protocol::model::QuickCommandKind,
        body: String,
    ) -> Result<(), String> {
        self.ask_result(Request::SaveQuickCommand {
            id: None,
            project,
            name,
            kind,
            body,
        })
        .await
        .map(|_| ())
    }

    /// Scheduled jobs for a project, or every one.
    pub async fn cron_jobs(
        &self,
        project: Option<ProjectName>,
    ) -> Vec<ginka_protocol::model::CronJob> {
        match self.ask(Request::ListCronJobs { project }).await {
            Some(Response::CronJobs { jobs }) => jobs,
            _ => Vec::new(),
        }
    }

    /// Save a scheduled job; the refusal is the daemon's own words.
    pub async fn save_cron_job(&self, request: Request) -> Result<(), String> {
        self.ask_result(request).await.map(|_| ())
    }

    /// Forget a scheduled job.
    pub async fn remove_cron_job(&self, id: i64) {
        self.ask(Request::RemoveCronJob { id }).await;
    }

    /// Fire a scheduled job now.
    pub async fn run_cron_job(&self, id: i64) -> Result<(), String> {
        self.ask_result(Request::RunCronJob { id })
            .await
            .map(|_| ())
    }

    /// Forget a quick command.
    pub async fn remove_quick_command(&self, id: String) {
        self.ask(Request::RemoveQuickCommand { id }).await;
    }

    /// Run a shell quick command in a new terminal in the workspace.
    pub async fn run_quick_command(
        &self,
        workspace: &WorkspaceId,
        id: String,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalId, String> {
        match self
            .ask_result(Request::RunQuickCommand {
                workspace: workspace.clone(),
                id,
                rows,
                cols,
            })
            .await?
        {
            Response::Terminal { terminal } => Ok(terminal),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// Run the conversation again from an edited prompt; answers with the
    /// session it now continues in.
    pub async fn edit_prompt(
        &self,
        session: &SessionId,
        seq: u64,
        text: String,
    ) -> Result<ginka_protocol::model::Session, String> {
        match self
            .ask_result(Request::EditPrompt {
                session: session.clone(),
                seq,
                text,
            })
            .await?
        {
            Response::Session { session } => Ok(session),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// Queue a follow-up behind the running turn rather than steering it in.
    pub async fn queue_message(&self, session: &SessionId, text: String) -> Result<(), String> {
        self.git_sync(Request::QueueMessage {
            session: session.clone(),
            text,
        })
        .await
    }

    /// Stop the running turn and send this queued follow-up next.
    pub async fn interrupt_with_queued(&self, session: &SessionId, id: u64) -> Result<(), String> {
        self.git_sync(Request::InterruptWithQueuedMessage {
            session: session.clone(),
            id,
        })
        .await
    }

    /// Hold the queue, or let it go.
    pub async fn set_queue_paused(&self, session: &SessionId, paused: bool) -> Result<(), String> {
        self.git_sync(Request::SetQueuePaused {
            session: session.clone(),
            paused,
        })
        .await
    }

    /// Throw away every queued follow-up.
    pub async fn clear_queue(&self, session: &SessionId) -> Result<(), String> {
        self.git_sync(Request::ClearQueue {
            session: session.clone(),
        })
        .await
    }

    /// Replace one waiting follow-up.
    pub async fn edit_queued_message(
        &self,
        session: &SessionId,
        id: u64,
        text: String,
    ) -> Result<(), String> {
        self.git_sync(Request::EditQueuedMessage {
            session: session.clone(),
            id,
            text,
        })
        .await
    }

    /// Remove one waiting follow-up.
    pub async fn remove_queued_message(&self, session: &SessionId, id: u64) -> Result<(), String> {
        self.git_sync(Request::RemoveQueuedMessage {
            session: session.clone(),
            id,
        })
        .await
    }

    /// Move one waiting follow-up to a zero-based dispatch position.
    pub async fn move_queued_message(
        &self,
        session: &SessionId,
        id: u64,
        index: u32,
    ) -> Result<(), String> {
        self.git_sync(Request::MoveQueuedMessage {
            session: session.clone(),
            id,
            index,
        })
        .await
    }

    /// Inject one waiting follow-up into the active turn when supported.
    pub async fn send_queued_message_now(
        &self,
        session: &SessionId,
        id: u64,
    ) -> Result<(), String> {
        self.git_sync(Request::SendQueuedMessageNow {
            session: session.clone(),
            id,
        })
        .await
    }

    /// Ask the provider to compact an idle conversation's context.
    pub async fn compact_session(&self, session: &SessionId) -> Result<(), String> {
        match self
            .ask_result(Request::CompactSession {
                session: session.clone(),
            })
            .await
        {
            Ok(Response::Ack) => Ok(()),
            Ok(other) => Err(format!("unexpected compact response: {other:?}")),
            Err(error) => Err(error),
        }
    }

    /// Copy a conversation through a transcript position onto another agent.
    pub async fn fork_session(
        &self,
        session: &SessionId,
        after: u64,
        agent: String,
    ) -> Result<Session, String> {
        match self
            .ask_result(Request::ForkSession {
                session: session.clone(),
                after: Some(after),
                agent: Some(agent),
                model: None,
                account: None,
            })
            .await?
        {
            Response::Session { session } => Ok(session),
            _ => Err("the daemon returned the wrong response for a session fork".into()),
        }
    }

    /// Read local branches and which worktree, if any, currently holds each.
    pub async fn branches(&self, workspace: &WorkspaceId) -> Result<Vec<BranchInfo>, String> {
        match self
            .ask_result(Request::ListBranches {
                workspace: workspace.clone(),
            })
            .await?
        {
            Response::Branches { branches } => Ok(branches),
            _ => Err("the daemon returned the wrong response for branch listing".into()),
        }
    }

    /// Switch a workspace without changing its immutable workspace id.
    pub async fn checkout_branch(
        &self,
        workspace: &WorkspaceId,
        branch: String,
        create: bool,
    ) -> Result<(), String> {
        match self
            .ask_result(Request::CheckoutBranch {
                workspace: workspace.clone(),
                branch,
                create,
            })
            .await?
        {
            Response::Ack => Ok(()),
            _ => Err("the daemon returned the wrong response for branch checkout".into()),
        }
    }

    /// Ask the daemon one question and keep its refusal, for the callers
    /// that show one: a refused request is an answer, not a lost connection.
    /// Push the branch and open a pull request for it. The refusal is the
    /// one `gh` or git gave, because it is the one the reader can act on.
    pub async fn create_pull_request(&self, workspace: &WorkspaceId) -> Result<String, String> {
        match self
            .ask_result(Request::CreatePullRequest {
                workspace: workspace.clone(),
                draft: false,
            })
            .await?
        {
            Response::PullRequest { url } => Ok(url),
            other => Err(format!("unexpected answer: {other:?}")),
        }
    }

    /// A project's notes, or every note.
    pub async fn notes(&self, project: Option<ProjectName>) -> Vec<Note> {
        match self.ask(Request::ListNotes { project }).await {
            Some(Response::Notes { notes }) => notes,
            _ => Vec::new(),
        }
    }

    /// Write a note, new or not.
    pub async fn save_note(
        &self,
        id: Option<String>,
        project: Option<ProjectName>,
        title: String,
        body: String,
    ) -> Option<Note> {
        match self
            .ask(Request::SaveNote {
                id,
                project,
                title,
                body,
            })
            .await
        {
            Some(Response::Note { note }) => Some(note),
            _ => None,
        }
    }

    /// Forget a note.
    pub async fn remove_note(&self, id: String) {
        self.ask(Request::RemoveNote { id }).await;
    }

    /// Pin or unpin a workspace; the refusal is the daemon's own words.
    pub async fn pin_workspace(&self, workspace: &WorkspaceId, pinned: bool) -> Result<(), String> {
        self.ask_result(Request::PinWorkspace {
            workspace: workspace.clone(),
            pinned,
        })
        .await
        .map(|_| ())
    }

    /// Archive a workspace, or bring it back.
    pub async fn archive_workspace(
        &self,
        workspace: &WorkspaceId,
        archived: bool,
    ) -> Result<(), String> {
        self.ask_result(Request::ArchiveWorkspace {
            workspace: workspace.clone(),
            archived,
        })
        .await
        .map(|_| ())
    }

    /// Give a conversation a title; an empty one clears it back to none.
    /// Show a project under another name; blank goes back to its own.
    pub async fn set_project_label(
        &self,
        project: &ProjectName,
        label: String,
    ) -> Result<(), String> {
        self.ask_result(Request::SetProjectLabel {
            project: project.clone(),
            label,
        })
        .await
        .map(|_| ())
    }

    /// Put a project at `index` in the rail.
    pub async fn move_project(&self, project: &ProjectName, index: u32) -> Result<(), String> {
        self.ask_result(Request::MoveProject {
            project: project.clone(),
            index,
        })
        .await
        .map(|_| ())
    }

    pub async fn rename_session(&self, session: &SessionId, title: String) -> Result<(), String> {
        self.ask_result(Request::RenameSession {
            session: session.clone(),
            title,
        })
        .await
        .map(|_| ())
    }

    async fn ask_result(&self, request: Request) -> Result<Response, String> {
        let client = self
            .client()
            .await
            .ok_or_else(|| rust_i18n::t!("daemon.unreachable").to_string())?;
        client.request(request).await.map_err(|error| {
            // The daemon answered — it just said no — so the connection is
            // still good and is kept.
            error.message
        })
    }

    /// Ask the daemon one question, reconnecting next time if it fails.
    async fn ask(&self, request: Request) -> Option<Response> {
        let client = self.client().await?;
        match client.request(request).await {
            Ok(response) => Some(response),
            Err(error) => {
                tracing::warn!(%error, "the daemon refused a request");
                // Whatever went wrong, the next request reconnects rather than
                // retrying down a socket that may already be closed.
                self.forget();
                None
            }
        }
    }

    /// The live connection, opening one if there is none.
    async fn client(&self) -> Option<Arc<Client>> {
        if let Some(client) = self.cached() {
            return Some(client);
        }
        match self.discovery.connect(None).await {
            Ok(client) => {
                let client = Arc::new(client);
                *self.lock() = Some(client.clone());
                tracing::info!(version = client.daemon_version(), "connected to the daemon");
                Some(client)
            }
            Err(error) => {
                tracing::error!(%error, "could not reach a daemon");
                None
            }
        }
    }

    fn cached(&self) -> Option<Arc<Client>> {
        self.lock().clone()
    }

    fn forget(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<Client>>> {
        self.client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Unix seconds, for the relative ages the sidebar shows.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
