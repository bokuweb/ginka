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

        let handshake = Handshake {
            port,
            // A v4 uuid is 122 bits of randomness from the OS; the token only
            // has to be unguessable by another local process.
            token: uuid::Uuid::new_v4().simple().to_string(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
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
            Box::pin(accepting),
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

/// One client connection.
struct Connection {
    hub: Arc<Hub>,
    service: Arc<Mutex<Service>>,
    token: String,
    version: String,
}

impl Connection {
    async fn serve(self, stream: async_net::TcpStream) -> Result<()> {
        let socket = self.accept(stream).await?;
        let (mut sink, mut incoming) = socket.split();

        // Everything written to the socket goes through one channel, so the
        // reader and the event pump never interleave halves of a frame.
        let (outbound, to_write) = async_channel::unbounded::<ServerMessage>();
        let events = self.hub.subscribe();

        outbound
            .send(ServerMessage::Hello {
                version: self.version.clone(),
                seq: self.hub.current_seq(),
            })
            .await
            .ok();

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

        let pushing = {
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
        };

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
                for message in self.dispatch(&text).await {
                    if outbound.send(message).await.is_err() {
                        return;
                    }
                }
            }
        };

        // The writer is the one that decides when this connection is over.
        // Feeding it stops first — the client went away, or the daemon is
        // stopping — and closing the queue then lets the writer drain what is
        // already in it before it ends. Without that ordering the answer to
        // the request that asked for a shutdown is written into a socket that
        // has already been torn down, and the client sees a closed connection
        // instead of its reply.
        let feeding = async {
            futures_util::future::select(Box::pin(reading), Box::pin(pushing)).await;
            outbound.close();
        };
        futures_util::future::join(Box::pin(feeding), Box::pin(writing)).await;
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
    async fn dispatch(&self, text: &str) -> Vec<ServerMessage> {
        let message: ClientMessage = match serde_json::from_str(text) {
            Ok(message) => message,
            Err(error) => {
                // The id is recovered from the raw frame so the client that
                // sent it gets an answer. This is the case of a client newer
                // than its daemon — a method this build has never heard of —
                // and answering id 0 would leave that client waiting forever
                // for a reply that had already been sent.
                return vec![ServerMessage::Error {
                    id: recover_request_id(text),
                    error: RpcError::malformed(error.to_string()),
                }];
            }
        };

        match message {
            ClientMessage::Request { id, payload } => {
                vec![match self.handle(payload).await {
                    Ok(response) => ServerMessage::Response {
                        id,
                        payload: response,
                    },
                    Err(error) => ServerMessage::Error { id, error },
                }]
            }
            ClientMessage::Resume { after } => match self.hub.replay_after(after) {
                Ok(entries) => entries
                    .into_iter()
                    .map(|entry| ServerMessage::Event {
                        seq: entry.seq,
                        payload: entry.event,
                    })
                    .collect(),
                Err(gap) => vec![ServerMessage::Gap { oldest: gap.oldest }],
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
