//! The WebSocket RPC server.
//!
//! Loopback only, bearer-token authenticated, one JSON frame per message. The
//! server decides nothing: it authenticates, frames, sequences and hands the
//! request to `ginka-core`'s `Service`.
//!
//! Concurrency is per connection. Requests from one client are handled on the
//! blocking pool — git shells out and SQLite is synchronous — while the
//! connection's writer keeps draining pushes, so a slow `git status` cannot
//! stall the event stream.

use crate::handshake;
use crate::hub::Hub;
use anyhow::{Context, Result};
use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as HandshakeRequest, Response as HandshakeResponse,
};
use async_tungstenite::tungstenite::http::StatusCode;
use futures_util::StreamExt;
use ginka_core::Paths;
use ginka_core::service::Service;
use ginka_core::settings::DaemonSettings;
use ginka_protocol::{
    ClientMessage, DaemonEvent, Handshake, RpcError, ServerMessage, rpc::Request,
};
use std::sync::{Arc, Mutex};

/// How long a stopping daemon waits for its connections to flush.
///
/// Bounded: a client that has stopped reading its socket must not be able to
/// keep the daemon alive. The ordering is what makes the wait correct; this is
/// only the limit on how long correctness is worth waiting for.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How many events the daemon keeps for reconnecting clients.
///
/// A client that has been away longer than this re-reads instead of patching.
/// Sized for a UI that was closed for a few minutes of agent output, not for
/// one that was closed overnight.
const EVENT_WINDOW: usize = 4096;

/// A bound, published daemon that has not started accepting yet.
pub struct Daemon {
    listener: async_net::TcpListener,
    hub: Arc<Hub>,
    service: Arc<Mutex<Service>>,
    handshake: Handshake,
    paths: Paths,
    /// Connections that have not finished writing yet.
    live: Arc<std::sync::atomic::AtomicUsize>,
    /// How often worktrees are reconciled against git.
    sync_every: std::time::Duration,
    /// How often each workspace's git status is re-read.
    status_every: std::time::Duration,
}

impl Daemon {
    /// Bind a loopback port, open the database and publish the handshake file.
    ///
    /// Binding before serving is what lets a caller — the test suite, or a
    /// client that spawned this daemon — know the port without racing the
    /// accept loop. A `port` of 0 in the settings asks the OS for a free one.
    pub fn bind(paths: Paths, settings: DaemonSettings) -> Result<Self> {
        paths.ensure()?;
        let handshake_path = paths.daemon_handshake();
        if let Some(existing) = handshake::read(&handshake_path)
            && is_listening(existing.port)
        {
            anyhow::bail!(
                "a daemon is already running (pid {}) on port {}",
                existing.pid,
                existing.port
            );
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", settings.port))
            .with_context(|| format!("binding 127.0.0.1:{}", settings.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let listener = async_net::TcpListener::try_from(listener)?;

        let epoch = ginka_core::daemon::new_epoch();
        let handshake = Handshake {
            protocol_version: ginka_protocol::envelope::PROTOCOL_VERSION,
            port,
            // A v4 uuid is 122 bits of randomness from the OS; the token only
            // has to be unguessable by another local process.
            token: uuid::Uuid::new_v4().simple().to_string(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            epoch,
        };

        let hub = Arc::new(Hub::new(EVENT_WINDOW));
        let service = Service::open(paths.clone(), hub.clone())?
            .with_drivers(ginka_core::driver::Registry::from_settings(&settings));
        handshake::write(&handshake_path, &handshake)?;

        Ok(Self {
            listener,
            hub,
            service: Arc::new(Mutex::new(service)),
            handshake,
            paths,
            live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            sync_every: std::time::Duration::from_secs(settings.sync_interval_secs.max(1)),
            status_every: std::time::Duration::from_secs(settings.status_poll_secs.max(1)),
        })
    }

    /// The port and token clients need to connect.
    pub fn handshake(&self) -> Handshake {
        self.handshake.clone()
    }

    /// Accept connections until a client asks the daemon to stop.
    ///
    /// The handshake file is removed on the way out, so nothing is left
    /// pointing at a dead port.
    pub async fn serve(self) -> Result<()> {
        // Subscribed before the first connection is accepted: the shutdown
        // event is published on the request path, and a subscription created
        // later would miss it.
        let shutdown = self.hub.subscribe();
        let handshake_path = self.paths.daemon_handshake();
        tracing::info!(port = self.handshake.port, "daemon listening");

        let accepting = async {
            loop {
                match self.listener.accept().await {
                    Ok((stream, peer)) => {
                        let connection = Connection {
                            hub: self.hub.clone(),
                            service: self.service.clone(),
                            token: self.handshake.token.clone(),
                            version: self.handshake.version.clone(),
                            epoch: self.handshake.epoch,
                        };
                        let live = self.live.clone();
                        live.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        smol::spawn(async move {
                            if let Err(error) = connection.serve(stream).await {
                                tracing::debug!(%peer, %error, "connection closed");
                            }
                            live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                        })
                        .detach();
                    }
                    Err(error) => {
                        tracing::warn!(%error, "accept failed");
                        return;
                    }
                }
            }
        };

        // The daemon's own clock. A worktree added with the user's own git, a
        // branch that has moved, work left uncommitted -- every window wants
        // to know, and having each of them poll for it is the same work done
        // once per window.
        let watching = {
            let service = self.service.clone();
            let sync_every = self.sync_every;
            let status_every = self.status_every;
            async move {
                let mut next_sync = std::time::Instant::now() + sync_every;
                let mut next_status = std::time::Instant::now();
                loop {
                    let now = std::time::Instant::now();
                    let wake = next_sync.min(next_status);
                    if wake > now {
                        smol::Timer::at(wake).await;
                    }
                    let now = std::time::Instant::now();
                    let (sync, status) = (now >= next_sync, now >= next_status);
                    if sync {
                        next_sync = now + sync_every;
                    }
                    if status {
                        next_status = now + status_every;
                    }
                    // git shells out, so this goes to the blocking pool rather
                    // than holding the executor a request is being served on.
                    let service = service.clone();
                    smol::unblock(move || {
                        let mut service = service.lock().unwrap_or_else(|e| e.into_inner());
                        if sync {
                            service.sync();
                        }
                        if status {
                            service.poll_statuses();
                        }
                    })
                    .await;
                }
            }
        };

        let stopping = async {
            while let Some(entry) = shutdown.next().await {
                if entry.event == DaemonEvent::Shutdown {
                    return;
                }
            }
        };

        // `pkill ginka-daemon`, a logout, a machine shutting down. Without
        // this the daemon dies where it stands and leaves a handshake file
        // pointing at a port nothing answers.
        let signalled = async {
            match async_signal::Signals::new([
                async_signal::Signal::Term,
                async_signal::Signal::Int,
                async_signal::Signal::Hup,
            ]) {
                Ok(mut signals) => {
                    if let Some(Ok(signal)) = signals.next().await {
                        tracing::info!(?signal, "stopping on a signal");
                    }
                }
                Err(error) => {
                    // Without a handler the default disposition still applies,
                    // so the daemon stops -- just not tidily.
                    tracing::warn!(%error, "could not listen for signals");
                    std::future::pending::<()>().await;
                }
            }
        };

        futures_util::future::select(
            futures_util::future::select(Box::pin(accepting), Box::pin(watching)),
            futures_util::future::select(Box::pin(stopping), Box::pin(signalled)),
        )
        .await;

        // Closing the event streams ends every connection's pump, which closes
        // its write queue, which lets the writer flush what is already in it.
        // Then wait for those writers: the client that asked the daemon to stop
        // is owed the answer, and a daemon that exits first hands it a closed
        // connection instead.
        self.hub.close();
        let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
        while self.live.load(std::sync::atomic::Ordering::SeqCst) > 0
            && std::time::Instant::now() < deadline
        {
            smol::Timer::after(std::time::Duration::from_millis(5)).await;
        }

        handshake::remove(&handshake_path)?;
        tracing::info!("daemon stopped");
        Ok(())
    }
}

/// Dig the request id out of a frame that would not parse.
///
/// Only the id is wanted, and only well enough to answer: a frame that has no
/// readable id gets 0, which is the "nobody is waiting for this" id.
fn recover_request_id(text: &str) -> ginka_protocol::RequestId {
    #[derive(serde::Deserialize)]
    struct JustTheId {
        id: Option<ginka_protocol::RequestId>,
    }
    serde_json::from_str::<JustTheId>(text)
        .ok()
        .and_then(|frame| frame.id)
        .unwrap_or(0)
}

/// Whether something is accepting connections on a loopback port.
///
/// This is how a stale handshake file is told from a live daemon: probing the
/// pid would say only that *a* process exists, and pids are reused.
fn is_listening(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}

/// What one client frame produced.
struct Dispatched {
    /// What to answer with, in order.
    messages: Vec<ServerMessage>,
    /// Whether this frame asked the daemon to stop, in which case the
    /// connection ends once these have been queued.
    stop: bool,
}

impl Dispatched {
    /// One message, and nothing else to do.
    fn just(message: ServerMessage) -> Self {
        Self {
            messages: vec![message],
            stop: false,
        }
    }
}

/// One client connection.
struct Connection {
    hub: Arc<Hub>,
    service: Arc<Mutex<Service>>,
    token: String,
    version: String,
    /// This daemon run, which a client compares against the cursor it stored.
    epoch: u64,
}

impl Connection {
    async fn serve(self, stream: async_net::TcpStream) -> Result<()> {
        let socket = self.accept(stream).await?;
        let (mut sink, mut incoming) = socket.split();

        // Everything written to the socket goes through one channel, so the
        // reader and the event pump never interleave halves of a frame.
        let (outbound, to_write) = async_channel::unbounded::<ServerMessage>();
        let events = self.hub.subscribe();

        let writing = async move {
            while let Ok(message) = to_write.recv().await {
                let text = match serde_json::to_string(&message) {
                    Ok(text) => text,
                    Err(error) => {
                        tracing::error!(%error, "a server message would not serialize");
                        continue;
                    }
                };
                if sink.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
        };

        // Spawned rather than raced with the reader: the queue is closed by
        // whoever is still putting things in it, and that is the reader. A
        // pusher that closed it could do so between the reader handling a
        // request and queueing the answer to it, which is exactly how the
        // reply to `daemon stop` went missing.
        let pushing = smol::spawn({
            let outbound = outbound.clone();
            async move {
                while let Some(entry) = events.next().await {
                    let message = ServerMessage::Event {
                        seq: entry.seq,
                        payload: entry.event,
                    };
                    if outbound.send(message).await.is_err() {
                        return;
                    }
                }
            }
        });

        let reading = async {
            while let Some(frame) = incoming.next().await {
                let text = match frame {
                    Ok(Message::Text(text)) => text.to_string(),
                    Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Ok(Message::Close(_)) | Err(_) => return,
                    // Ping/Pong are answered by the library.
                    Ok(_) => continue,
                };
                let dispatched = self.dispatch(&text).await;
                for message in dispatched.messages {
                    if outbound.send(message).await.is_err() {
                        return;
                    }
                }
                // The client asked the daemon to stop, and has been answered.
                // Ending here is what lets the writer flush that answer before
                // the process goes.
                if dispatched.stop {
                    return;
                }
            }
        };

        // The writer is the one that decides when this connection is over.
        // The reader stops first — the client went away, or it asked the
        // daemon to stop and has been answered — and closing the queue then
        // lets the writer drain what is already in it. Without that ordering
        // the answer to `daemon stop` is written into a socket that has
        // already been torn down, and the client sees a closed connection
        // instead of its reply.
        let feeding = async {
            reading.await;
            outbound.close();
        };
        futures_util::future::join(Box::pin(feeding), Box::pin(writing)).await;
        // Nothing left to push into.
        drop(pushing);
        Ok(())
    }

    /// Complete the WebSocket upgrade, refusing anything without the token.
    // The callback's `Result<Response, ErrorResponse>` is tungstenite's
    // signature, so the size of the error variant is not ours to choose.
    #[allow(clippy::result_large_err)]
    async fn accept(
        &self,
        stream: async_net::TcpStream,
    ) -> Result<WebSocketStream<async_net::TcpStream>> {
        let expected = format!("Bearer {}", self.token);
        let socket = async_tungstenite::accept_hdr_async(
            stream,
            move |request: &HandshakeRequest, response: HandshakeResponse| {
                let presented = request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                // Loopback is not authentication: every other process on the
                // machine can reach this port.
                if presented == expected {
                    Ok(response)
                } else {
                    let mut refusal =
                        ErrorResponse::new(Some("missing or wrong bearer token".into()));
                    *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                    Err(refusal)
                }
            },
        )
        .await
        .context("websocket upgrade")?;
        Ok(socket)
    }

    /// Handle one client frame, returning everything it should answer with.
    async fn dispatch(&self, text: &str) -> Dispatched {
        let message: ClientMessage = match serde_json::from_str(text) {
            Ok(message) => message,
            Err(error) => {
                // The id is recovered from the raw frame so the client that
                // sent it gets an answer. This is the case of a client newer
                // than its daemon — a method this build has never heard of —
                // and answering id 0 would leave that client waiting forever
                // for a reply that had already been sent.
                return Dispatched::just(ServerMessage::Error {
                    id: recover_request_id(text),
                    error: RpcError::malformed(error.to_string()),
                });
            }
        };

        match message {
            // The version is checked before the token, so a client on the
            // wrong contract is told that rather than sent chasing a token
            // problem it does not have.
            ClientMessage::Hello { .. } => Dispatched::just(ginka_core::daemon::greet(
                &self.token,
                self.epoch,
                self.hub.current_seq(),
                &self.version,
                &message,
            )),
            ClientMessage::Request { id, payload } => {
                let stop = matches!(payload, Request::Shutdown);
                let answer = match self.handle(payload).await {
                    Ok(response) => ServerMessage::Response {
                        id,
                        payload: response,
                    },
                    Err(error) => ServerMessage::Error { id, error },
                };
                Dispatched {
                    messages: vec![answer],
                    stop,
                }
            }
            ClientMessage::Resume { after } => Dispatched {
                messages: match self.hub.replay_after(after) {
                    Ok(entries) => entries
                        .into_iter()
                        .map(|entry| ServerMessage::Event {
                            seq: entry.seq,
                            payload: entry.event,
                        })
                        .collect(),
                    Err(gap) => vec![ServerMessage::Gap { oldest: gap.oldest }],
                },
                stop: false,
            },
        }
    }

    /// Run one request on the blocking pool.
    ///
    /// `Service` shells out to git and talks to SQLite, so it must not run on
    /// an executor thread that other connections' pushes are waiting on.
    async fn handle(&self, request: Request) -> Result<ginka_protocol::rpc::Response, RpcError> {
        let service = self.service.clone();
        smol::unblock(move || {
            let mut service = service
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            service.handle(request)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_that_will_not_parse_is_still_answered_to_its_sender() {
        // A client newer than its daemon sends a method this build has never
        // heard of. Answering id 0 leaves that client waiting for a reply it
        // has already been sent -- which is what a `ginka` that outran its
        // running daemon looks like: a command that hangs.
        assert_eq!(
            recover_request_id(
                r#"{"type":"request","id":7,"payload":{"method":"from_the_future"}}"#
            ),
            7
        );
    }

    #[test]
    fn a_frame_with_no_id_at_all_is_answered_to_nobody() {
        assert_eq!(recover_request_id("not json"), 0);
        assert_eq!(recover_request_id(r#"{"type":"resume"}"#), 0);
        assert_eq!(recover_request_id(r#"{"id":"not a number"}"#), 0);
    }
}
