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
use ginka_protocol::model::{AgentStatus, Session, TranscriptEntry};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{SessionId, WorkspaceId};
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
    ) -> Option<Session> {
        match self
            .ask(Request::StartSession {
                workspace: workspace.clone(),
                agent: agent.to_string(),
                prompt,
                model: None,
            })
            .await
        {
            Some(Response::Session { session }) => Some(session),
            _ => None,
        }
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
