//! What actually travels over the socket.
//!
//! Every frame is one JSON object with a `type` discriminator. Requests carry
//! an id the response echoes; pushes carry a sequence number so a client that
//! reconnects can ask for the gap instead of re-reading the world.

use crate::event::DaemonEvent;
use crate::rpc::{Request, Response};
use serde::{Deserialize, Serialize};

/// The contract version this build speaks. A mismatch fails the handshake
/// loudly: a client and daemon that disagree about the wire will otherwise
/// half-work, which is far harder to diagnose than a refusal.
pub const PROTOCOL_VERSION: u32 = 48;

/// Largest message either side will accept. Attachments travel over this
/// socket, so the cap has to clear the largest upload the daemon takes — and
/// exist at all, so a corrupt length cannot ask us to allocate a gigabyte.
pub const MAX_WIRE_MESSAGE_BYTES: usize = 48 * 1024 * 1024;

/// Monotonic per-daemon sequence number. Server pushes carry one so a
/// reconnecting client can ask for everything after the last seq it saw
/// instead of re-reading the world.
pub type Seq = u64;

/// Correlates a request with its response. Unique per connection only.
pub type RequestId = u64;

/// A frame from a client to the daemon.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on every connection. Nothing else is answered until it
    /// has been accepted: a connection that has not identified itself has no
    /// business asking questions.
    Hello {
        /// The [`PROTOCOL_VERSION`] the client was built against.
        protocol_version: u32,
        /// The daemon's bearer token, as the handshake file records it.
        token: String,
    },
    /// A request expecting exactly one [`ServerMessage::Response`] or
    /// [`ServerMessage::Error`] with the same `id`.
    Request {
        /// Echoed on the answer, so replies can arrive out of order.
        id: RequestId,
        /// What the client is asking for.
        payload: Box<Request>,
    },
    /// Replay the event stream from after `after`.
    ///
    /// Sent immediately after connecting by a client that has state to catch
    /// up; a fresh client sends `after: 0` or nothing at all.
    Resume {
        /// The last sequence number the client has applied; `0` asks for
        /// everything still replayable.
        after: Seq,
    },
}

/// A frame from the daemon to a client.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The handshake was accepted.
    ///
    /// `seq` is the daemon's current position, which is what a client with no
    /// stored cursor resumes from — it wants what happens next, not the
    /// backlog of a daemon that has been running for a week. `epoch`
    /// identifies this daemon *run*: a client whose cursor belongs to an
    /// earlier run has to re-read the world rather than resume, because
    /// sequence numbers start again per run.
    Welcome {
        /// The [`PROTOCOL_VERSION`] the daemon speaks.
        protocol_version: u32,
        /// The daemon's build version, for display and bug reports.
        version: String,
        /// Identifies this daemon run; changes on every restart.
        epoch: u64,
        /// The daemon's current event position.
        seq: Seq,
    },
    /// The handshake was refused, with the reason named so the client can say
    /// something better than "could not connect".
    Rejected {
        /// Why the handshake was refused.
        reason: HandshakeRejection,
    },
    /// A successful answer to the request with this `id`.
    Response {
        /// The `id` of the request this answers.
        id: RequestId,
        /// The answer itself.
        payload: Box<Response>,
    },
    /// A failed answer. Kept separate from [`ServerMessage::Response`] because
    /// `Result` serialises as `{"Ok":…}`/`{"Err":…}`, which is a Rust detail no
    /// other client should have to know about.
    Error {
        /// The `id` of the request that failed.
        id: RequestId,
        /// What went wrong.
        error: RpcError,
    },
    /// An unsolicited push. Every event carries a `seq` so gaps are detectable.
    Event {
        /// Position of this push in the daemon's stream.
        seq: Seq,
        /// What happened.
        payload: DaemonEvent,
    },
    /// The cursor in a [`ClientMessage::Resume`] is older than the daemon's
    /// replay window, so the events in between are gone.
    ///
    /// A client that gets this must re-read what it cares about rather than
    /// carrying on patching: a short replay would leave it believing it was
    /// caught up. `oldest` is the earliest sequence number still replayable.
    Gap {
        /// The earliest sequence number still replayable.
        oldest: Seq,
    },
}

/// Why a connection was refused.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HandshakeRejection {
    /// The daemon speaks a different contract. Carries the daemon's version so
    /// the client can tell the user which side is behind.
    VersionMismatch {
        /// The [`PROTOCOL_VERSION`] the daemon speaks.
        daemon: u32,
    },
    /// Wrong or missing token. Deliberately says nothing else.
    BadToken,
    /// Something other than a hello arrived first.
    HelloExpected,
}

impl ServerMessage {
    /// Whether this is an accepted handshake, without the caller having to
    /// match a message shape it does not otherwise care about.
    pub fn is_welcome(&self) -> bool {
        matches!(self, Self::Welcome { .. })
    }
}

/// Why a request failed.
///
/// `code` is stable and matchable; `message` is for a human and may change.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// Stable, machine-matchable reason: `not_found`, `failed` or `malformed`.
    pub code: String,
    /// Human-readable detail; wording may change between builds.
    pub message: String,
}

impl RpcError {
    /// The request named something that does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found".into(),
            message: message.into(),
        }
    }

    /// The request was well-formed but could not be carried out.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "failed".into(),
            message: message.into(),
        }
    }

    /// The frame could not be parsed as a request.
    pub fn malformed(message: impl Into<String>) -> Self {
        Self {
            code: "malformed".into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resume_with_no_cursor_asks_for_everything_after_the_start() {
        let parsed: ClientMessage = serde_json::from_str(r#"{"type":"resume","after":0}"#).unwrap();
        assert_eq!(parsed, ClientMessage::Resume { after: 0 });
    }

    #[test]
    fn a_gap_names_the_oldest_event_the_daemon_can_still_replay() {
        let wired = serde_json::to_value(ServerMessage::Gap { oldest: 12 }).unwrap();
        assert_eq!(wired["type"], serde_json::json!("gap"));
        assert_eq!(wired["oldest"], serde_json::json!(12));
    }

    #[test]
    fn error_codes_are_stable_enough_to_match_on() {
        assert_eq!(RpcError::not_found("x").code, "not_found");
        assert_eq!(RpcError::failed("x").code, "failed");
        assert_eq!(RpcError::malformed("x").code, "malformed");
    }
}
