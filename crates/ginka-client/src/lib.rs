//! Client for the Ginka daemon's WebSocket RPC.
//!
//! The GPUI app and the `ginka` CLI both talk to the daemon through this crate
//! and nothing else. That constraint is what makes every UI capability
//! scriptable — see `AGENTS.md` rule 3.
//!
//! [`Discovery`] finds a running daemon — or starts one — and hands back a
//! [`Client`]. A `Client` owns one connection and a task pumping it. Requests are
//! correlated by id, so callers may have several in flight; pushes arrive on a
//! channel whether or not anyone is asking for anything.

pub mod discovery;

pub use discovery::Discovery;

use anyhow::{Context, Result, anyhow};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::client::IntoClientRequest;
use async_tungstenite::tungstenite::http::HeaderValue;
use futures_util::StreamExt;
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{
    ClientMessage, DaemonEvent, Handshake, RequestId, RpcError, Seq, ServerMessage,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// One push from the daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The daemon-wide position of this event. Store it: it is what a
    /// reconnecting client resumes from.
    pub seq: Seq,
    pub payload: DaemonEvent,
}

/// Where a reconnecting client resumes the event stream from.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResumeCursor {
    pub after: Seq,
}

/// A live connection to the daemon.
///
/// Dropping it closes the connection; the daemon keeps running.
pub struct Client {
    outgoing: async_channel::Sender<ClientMessage>,
    events: async_channel::Receiver<Event>,
    pending: Pending,
    next_id: AtomicU64,
    current_seq: Seq,
    version: String,
    resync_needed: Arc<AtomicBool>,
    /// Dropping the task tears the connection down with the client.
    _pump: smol::Task<()>,
}

/// Requests waiting for an answer, by id.
type Pending = Arc<Mutex<HashMap<RequestId, async_channel::Sender<Result<Response, RpcError>>>>>;

impl Client {
    /// Connect and authenticate.
    ///
    /// `resume_from` replays everything the daemon has published since that
    /// sequence number. Pass `None` on a first connection: a client with no
    /// state wants what happens next, not the backlog of a daemon that has
    /// been running for a week.
    pub async fn connect(handshake: &Handshake, resume_from: Option<Seq>) -> Result<Self> {
        let mut request = handshake
            .endpoint()
            .into_client_request()
            .context("building the upgrade request")?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&handshake.authorization())?,
        );

        let stream = async_net::TcpStream::connect(("127.0.0.1", handshake.port))
            .await
            .with_context(|| format!("connecting to 127.0.0.1:{}", handshake.port))?;
        let (socket, _) = async_tungstenite::client_async(request, stream)
            .await
            .context("the daemon refused the connection")?;
        let (mut sink, mut incoming) = socket.split();

        // The daemon greets an authenticated connection before anything else,
        // and the greeting carries the position the stream is at.
        let (version, current_seq) = match next_message(&mut incoming).await {
            Some(ServerMessage::Hello { version, seq }) => (version, seq),
            other => {
                return Err(anyhow!(
                    "the daemon did not greet the connection: {other:?}"
                ));
            }
        };

        let (outgoing, to_send) = async_channel::unbounded::<ClientMessage>();
        let (event_sender, events) = async_channel::unbounded::<Event>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let resync_needed = Arc::new(AtomicBool::new(false));

        if let Some(after) = resume_from {
            outgoing.send(ClientMessage::Resume { after }).await.ok();
        }

        let pump = {
            let pending = pending.clone();
            let resync_needed = resync_needed.clone();
            smol::spawn(async move {
                let writing = async move {
                    while let Ok(message) = to_send.recv().await {
                        let Ok(text) = serde_json::to_string(&message) else {
                            continue;
                        };
                        if futures_util::SinkExt::send(&mut sink, Message::text(text))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                };

                let reading = async move {
                    while let Some(message) = next_message(&mut incoming).await {
                        match message {
                            ServerMessage::Response { id, payload } => {
                                answer(&pending, id, Ok(payload));
                            }
                            ServerMessage::Error { id, error } => {
                                answer(&pending, id, Err(error));
                            }
                            ServerMessage::Event { seq, payload } => {
                                if event_sender.send(Event { seq, payload }).await.is_err() {
                                    return;
                                }
                            }
                            ServerMessage::Gap { oldest } => {
                                // Everything before `oldest` is gone, so the
                                // client's view is stale in ways it cannot
                                // patch. Say so; re-reading is the caller's.
                                tracing::warn!(oldest, "missed events; a re-read is needed");
                                resync_needed.store(true, Ordering::Relaxed);
                            }
                            ServerMessage::Hello { .. } => {}
                        }
                    }
                    // The socket is gone: fail everything still waiting rather
                    // than leaving a caller blocked forever.
                    let waiting: Vec<_> = pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .drain()
                        .collect();
                    for (_, sender) in waiting {
                        sender
                            .send(Err(RpcError::failed("the daemon closed the connection")))
                            .await
                            .ok();
                    }
                };

                futures_util::future::select(Box::pin(writing), Box::pin(reading)).await;
            })
        };

        Ok(Self {
            outgoing,
            events,
            pending,
            next_id: AtomicU64::new(1),
            current_seq,
            version,
            resync_needed,
            _pump: pump,
        })
    }

    /// The daemon's version, from its greeting.
    pub fn daemon_version(&self) -> &str {
        &self.version
    }

    /// Where the daemon's event stream was when this client connected.
    pub fn current_seq(&self) -> Seq {
        self.current_seq
    }

    /// Whether a resume fell outside the daemon's replay window, so this
    /// client's view has to be re-read rather than patched.
    pub fn needs_resync(&self) -> bool {
        self.resync_needed.load(Ordering::Relaxed)
    }

    /// Send a request and wait for its answer.
    ///
    /// Several requests may be in flight at once: each carries an id and the
    /// daemon is free to answer them in any order.
    pub async fn request(&self, request: Request) -> Result<Response, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = async_channel::bounded(1);
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, sender);

        if self
            .outgoing
            .send(ClientMessage::Request {
                id,
                payload: request,
            })
            .await
            .is_err()
        {
            self.forget(id);
            return Err(RpcError::failed("the connection to the daemon is closed"));
        }

        match receiver.recv().await {
            Ok(result) => result,
            Err(_) => {
                self.forget(id);
                Err(RpcError::failed("the daemon closed the connection"))
            }
        }
    }

    /// The next push, waiting for one if none has arrived.
    pub async fn next_event(&self) -> Option<Event> {
        self.events.recv().await.ok()
    }

    /// The next push, if one is already waiting.
    pub fn try_next_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// The channel pushes arrive on, for a caller that wants to select on it.
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }

    fn forget(&self, id: RequestId) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
    }
}

/// Deliver an answer to whoever is waiting for it.
///
/// A missing entry means the caller gave up; that is not an error, and the
/// answer is dropped.
fn answer(pending: &Pending, id: RequestId, result: Result<Response, RpcError>) {
    let sender = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&id);
    if let Some(sender) = sender {
        sender.try_send(result).ok();
    }
}

/// Read frames until one parses as a server message, or the socket ends.
///
/// Non-text frames and frames the daemon sends that this build does not
/// understand are skipped rather than closing the connection: a newer daemon
/// must be able to add a message type without breaking older clients.
async fn next_message<S>(incoming: &mut S) -> Option<ServerMessage>
where
    S: futures_util::Stream<Item = Result<Message, async_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(frame) = incoming.next().await {
        let text = match frame {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Binary(bytes)) => String::from_utf8(bytes.to_vec()).ok()?,
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        };
        match serde_json::from_str(&text) {
            Ok(message) => return Some(message),
            Err(error) => {
                tracing::debug!(%error, "ignoring a frame this build does not understand");
            }
        }
    }
    None
}
