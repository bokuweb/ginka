//! The two event streams: what a driver emits, and what the daemon pushes.
//!
//! [`AgentEvent`] is the normalization boundary. Every driver — Claude Code's
//! stream-json, Codex, an ACP peer — turns its vendor shape into these
//! variants, and nothing above `ginka-core::driver` knows which vendor it is
//! talking to (`AGENTS.md` rule 6).

use crate::ids::{ProjectName, SessionId, WorkspaceId};
use crate::model::{BranchStatus, Session, SessionState, TranscriptEntry};
use serde::{Deserialize, Serialize};

/// One normalized thing an agent did.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A fragment of assistant text. Deltas are emitted as they arrive and are
    /// never buffered into whole messages by the driver — the UI folds them,
    /// because it is the only layer that knows what is on screen.
    TextDelta { text: String },
    /// A fragment of the agent's reasoning, where the vendor exposes it.
    Reasoning { text: String },
    /// The agent invoked a tool. `id` correlates with the matching
    /// [`AgentEvent::ToolResult`]; vendors that do not supply one get a
    /// synthesised id from the driver, so the pairing always exists.
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The result of a tool call, truncated by the driver if the vendor sent
    /// something unbounded.
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// The agent is blocked on the user. `options` is empty for a free-text
    /// question.
    AskUser {
        id: String,
        question: String,
        options: Vec<String>,
    },
    /// The agent proposed a plan and wants it approved before acting.
    PlanProposal { id: String, plan: String },
    /// Token and cost accounting for the turn so far.
    Usage { usage: Usage },
    /// A turn boundary. Checkpoints are taken here.
    TurnEnd { turn: u32 },
    /// The session reached a terminal state; no further events will arrive.
    SessionResult {
        state: SessionState,
        summary: Option<String>,
    },
}

/// Token and cost accounting.
///
/// Counts are cumulative for the session as the vendor reports them, not
/// per-turn deltas — vendors disagree about which they emit, and the drivers
/// normalize to cumulative because it is the one that survives a dropped event.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    /// `None` when the vendor does not price the request.
    pub cost_usd: Option<f64>,
}

/// Something the daemon pushes to every connected client.
///
/// Pushes are how the CLI, the app and a second window stay in step without
/// polling. Each is delivered with a sequence number so a reconnecting client
/// can ask for everything after the last one it saw.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    /// A project was registered or removed.
    ProjectsChanged,
    /// The set of worktrees under `project` changed, or one of their branches
    /// did. Clients re-read rather than patching, because git is the authority.
    WorkspacesChanged { project: ProjectName },
    /// A poller found the workspace's git status different from last tick.
    WorkspaceStatusChanged {
        workspace: WorkspaceId,
        status: BranchStatus,
    },
    /// A session was created; carries the whole record so a client that has
    /// never seen it does not have to ask.
    SessionStarted { session: Session },
    /// Something was added to a transcript: a prompt the user sent, or an
    /// event a driver produced.
    ///
    /// The whole entry rather than the event alone, so a client can fold it in
    /// where it belongs without asking for the page it is in — and so a user's
    /// own prompt appears in every window as soon as it is sent, rather than
    /// on whatever tick notices it next.
    SessionEvent {
        session: SessionId,
        entry: TranscriptEntry,
    },
    /// A session moved: started working, blocked on the user, or ended.
    ///
    /// Every transition, not only the last one. A client that only heard about
    /// the end could not tell an agent that is thinking from one that has not
    /// started, and would wait for a poll to find out.
    SessionStateChanged {
        session: SessionId,
        state: SessionState,
    },
    /// The daemon is shutting down. Clients should stop reconnecting.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_and_its_result_are_correlated_by_id() {
        let call = AgentEvent::ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "a.rs" }),
        };
        let result = AgentEvent::ToolResult {
            id: "call_1".into(),
            output: "fn main() {}".into(),
            is_error: false,
        };
        let (
            AgentEvent::ToolCall { id: call_id, .. },
            AgentEvent::ToolResult { id: result_id, .. },
        ) = (&call, &result)
        else {
            unreachable!("constructed above")
        };
        assert_eq!(call_id, result_id);
    }

    #[test]
    fn usage_defaults_to_zero_with_no_price() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cost_usd, None);
    }

    #[test]
    fn a_daemon_event_round_trips() {
        let event = DaemonEvent::SessionEvent {
            session: SessionId("s-1".into()),
            entry: TranscriptEntry {
                seq: 3,
                at: 1_700_000_000,
                payload: crate::model::TranscriptPayload::Agent {
                    event: AgentEvent::TextDelta {
                        text: "hello".into(),
                    },
                },
            },
        };
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<DaemonEvent>(&text).unwrap(), event);
    }
}
