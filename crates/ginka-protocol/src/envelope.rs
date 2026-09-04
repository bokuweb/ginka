use serde::{Deserialize, Serialize};

/// The contract version this build speaks. A mismatch fails the handshake
/// loudly: a client and daemon that disagree about the wire will otherwise
/// half-work, which is far harder to diagnose than a refusal.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest message either side will accept. Attachments travel over this
/// socket, so the cap has to clear the largest upload the daemon takes — and
/// exist at all, so a corrupt length cannot ask us to allocate a gigabyte.
pub const MAX_WIRE_MESSAGE_BYTES: usize = 48 * 1024 * 1024;

/// Monotonic per-connection sequence number. Server pushes carry one so a
/// reconnecting client can ask for everything after the last seq it saw
/// instead of re-reading the world. See `docs/roadmap.md` §5 (M2).
pub type Seq = u64;

/// Correlates a request with its response.
pub type RequestId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on every connection. Nothing else is answered until it
    /// has been accepted.
    Hello {
        protocol_version: u32,
        /// Bearer token from `daemon.json`, or the environment override.
        token: String,
    },
    /// A request expecting exactly one `ServerMessage::Response`.
    Request {
        id: RequestId,
        payload: serde_json::Value,
    },
    /// Resume the event stream after a reconnect.
    Resume { after: Seq },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The handshake was accepted. `epoch` identifies this daemon *run*: a
    /// client whose cursor belongs to an earlier run has to re-read the world
    /// rather than resume, because sequence numbers start again per run.
    Welcome { protocol_version: u32, epoch: u64 },
    /// The handshake was refused, with the reason named so the client can say
    /// something better than "could not connect".
    Rejected { reason: HandshakeRejection },
    Response {
        id: RequestId,
        result: Result<serde_json::Value, RpcError>,
    },
    /// An unsolicited push. Every event carries a `seq` so gaps are detectable.
    Event {
        seq: Seq,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HandshakeRejection {
    /// The daemon speaks a different contract. Carries the daemon's version so
    /// the client can tell the user which side is behind.
    VersionMismatch { daemon: u32 },
    /// Wrong or missing token. Deliberately says nothing else.
    BadToken,
    /// Something other than a hello arrived first. Nothing is served on a
    /// connection that has not identified itself.
    HelloExpected,
}

impl ServerMessage {
    /// Whether this is an accepted handshake, without the caller having to
    /// match a message shape it does not otherwise care about.
    pub fn is_welcome(&self) -> bool {
        matches!(self, Self::Welcome { .. })
    }
}
