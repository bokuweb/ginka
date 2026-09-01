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

use ginka_client::{Client, Discovery};
use ginka_core::Paths;
use ginka_protocol::rpc::{Request, Response};
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
        let Some(client) = self.client().await else {
            return Vec::new();
        };
        match client
            .request(Request::ListWorkspaces { project: None })
            .await
        {
            Ok(Response::Workspaces { workspaces }) => workspaces
                .iter()
                .map(|summary| SessionRow::from_summary(summary, now))
                .collect(),
            Ok(other) => {
                tracing::error!(?other, "the daemon answered a workspace listing with this");
                Vec::new()
            }
            Err(error) => {
                tracing::warn!(%error, "could not list workspaces; dropping the connection");
                // Whatever went wrong, the next request reconnects rather than
                // retrying down a socket that may already be closed.
                self.forget();
                Vec::new()
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
