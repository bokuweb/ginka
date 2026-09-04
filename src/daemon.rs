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
use ginka_protocol::model::{
    AgentStatus, ChangeSource, Changes, Checkpoint, FileEntry, Session, SlashCommand,
    TranscriptEntry,
};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{CheckpointId, SessionId, WorkspaceId};
use ginka_ui::workspace::SessionRow;
use std::sync::{Arc, Mutex};

/// A daemon connection shared by everything in the window.
pub struct DaemonLink {
    discovery: Discovery,
    /// `None` until the first request, and again after one fails.
    client: Mutex<Option<Arc<Client>>>,
}

impl DaemonLink {
    /// Build a link to the daemon that owns `paths`.
    pub fn new(paths: &Paths) -> Arc<Self> {
        Arc::new(Self {
            discovery: Discovery::new(paths.daemon_handshake()).with_home(paths.root()),
            client: Mutex::new(None),
        })
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

    /// Start an agent in a workspace, and return the session it created.
    pub async fn start_session(
        &self,
        workspace: &WorkspaceId,
        agent: &str,
        prompt: String,
        model: Option<String>,
    ) -> Option<Session> {
        match self
            .ask(Request::StartSession {
                workspace: workspace.clone(),
                agent: agent.to_string(),
                prompt,
                model,
            })
            .await
        {
            Some(Response::Session { session }) => Some(session),
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
    pub async fn commit(&self, workspace: &WorkspaceId, message: String) -> Result<(), String> {
        let client = self.client().await.ok_or("no daemon")?;
        match client
            .request(Request::Commit {
                workspace: workspace.clone(),
                message,
                all: true,
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
