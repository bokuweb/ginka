//! What actually travels over the socket.
//!
//! Every frame is one JSON object with a `type` discriminator. Requests carry
//! an id the response echoes; pushes carry a sequence number so a client that
//! reconnects can ask for the gap instead of re-reading the world.

use crate::event::DaemonEvent;
use crate::rpc::{Request, Response};
use serde::{Deserialize, Serialize};

/// Monotonic per-daemon sequence number. Server pushes carry one so a
/// reconnecting client can ask for everything after the last seq it saw
/// instead of re-reading the world.
pub type Seq = u64;

/// Correlates a request with its response. Unique per connection only.
pub type RequestId = u64;

/// A frame from a client to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// A request expecting exactly one [`ServerMessage::Response`] or
    /// [`ServerMessage::Error`] with the same `id`.
    Request { id: RequestId, payload: Request },
    /// Replay the event stream from after `after`.
    ///
    /// Sent immediately after connecting by a client that has state to catch
    /// up; a fresh client sends `after: 0` or nothing at all.
    Resume { after: Seq },
}

/// A frame from the daemon to a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent once, unprompted, as soon as the connection is authenticated.
    ///
    /// `seq` is the daemon's current position, which is what a client with no
    /// stored cursor resumes from — it wants what happens next, not the
    /// backlog of a daemon that has been running for a week.
    Hello { version: String, seq: Seq },
    /// A successful answer to the request with this `id`.
    Response { id: RequestId, payload: Response },
    /// A failed answer. Kept separate from [`ServerMessage::Response`] because
    /// `Result` serialises as `{"Ok":…}`/`{"Err":…}`, which is a Rust detail no
    /// other client should have to know about.
    Error { id: RequestId, error: RpcError },
    /// An unsolicited push. Every event carries a `seq` so gaps are detectable.
    Event { seq: Seq, payload: DaemonEvent },
    /// The cursor in a [`ClientMessage::Resume`] is older than the daemon's
    /// replay window, so the events in between are gone.
    ///
    /// A client that gets this must re-read what it cares about rather than
    /// carrying on patching: a short replay would leave it believing it was
    /// caught up. `oldest` is the earliest sequence number still replayable.
    Gap { oldest: Seq },
}

/// Why a request failed.
///
/// `code` is stable and matchable; `message` is for a human and may change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
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
