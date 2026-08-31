use serde::{Deserialize, Serialize};

/// Monotonic per-connection sequence number. Server pushes carry one so a
/// reconnecting client can ask for everything after the last seq it saw
/// instead of re-reading the world. See `docs/roadmap.md` §5 (M2).
pub type Seq = u64;

/// Correlates a request with its response.
pub type RequestId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
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
