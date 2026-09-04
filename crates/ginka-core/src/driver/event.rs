//! The one event stream every driver normalizes into.
//!
//! Vendor-specific shapes stop at the driver boundary: nothing above this
//! module knows whether it is talking to Claude Code or Codex (`AGENTS.md`
//! rule 6). See `docs/roadmap.md` §4.5 for the list and why each entry exists.

use serde::{Deserialize, Serialize};

use crate::driver::ActivityItem;
use crate::usage::TokenTotals;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The session is live. Carries the provider's own session id, which is
    /// what a resume is built from.
    Connected {
        session_id: Option<String>,
        model: Option<String>,
    },
    /// Slash commands the provider defines for itself, merged with the ones on
    /// disk by `crate::composer` (§3.3 N4).
    Commands(Vec<String>),
    TurnStarted,
    TextDelta(String),
    Reasoning(String),
    ToolCall(ActivityItem),
    ToolResult(ActivityItem),
    /// The agent is waiting on the user.
    AskUser {
        question: String,
        options: Vec<String>,
    },
    PlanProposal {
        plan: String,
    },
    /// The agent wants to do something its access mode does not allow.
    Permission {
        request: String,
    },
    /// A steered message reached the running turn (§3.3 N1).
    SteerAccepted,
    SteerRejected {
        reason: Option<String>,
    },
    /// The provider's own name for this session (§3.3 N5).
    AgentTitle(String),
    Usage(TokenTotals),
    TurnEnd {
        outcome: TurnOutcome,
    },
    /// The provider's closing summary for a run.
    SessionResult {
        text: Option<String>,
        failed: bool,
    },
    ProcessExited {
        code: Option<i32>,
    },
    /// A shape this build does not understand.
    ///
    /// Loud but not fatal. A vendor adding a message type should appear in the
    /// transcript as "not understood" rather than end the session — and rather
    /// than be silently mis-parsed into something it is not (roadmap R6).
    Unsupported {
        shape: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Failed { reason: String },
    Cancelled,
}

/// What can go wrong reading a provider's stream.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// The agent wrote something that is not a message at all — a panic, a
    /// warning on stdout, a truncated line.
    #[error("the agent produced a line that is not JSON: {line}")]
    Malformed { line: String },
    /// A message we recognise, shaped in a way we do not. Names the field so
    /// the report says which part of the contract moved.
    #[error(
        "the agent's {shape} message has no {field}; this build may be too old \
         for the installed CLI"
    )]
    MissingField { shape: String, field: &'static str },
}

impl DriverError {
    /// Lines are put in the message, so bound them: a runaway process can emit
    /// a megabyte on one line and it would otherwise all land in the log.
    pub const MAX_REPORTED_LINE: usize = 300;

    pub fn malformed(line: &str) -> Self {
        let line = line.trim();
        let mut cut = Self::MAX_REPORTED_LINE.min(line.len());
        while cut > 0 && !line.is_char_boundary(cut) {
            cut -= 1;
        }
        Self::Malformed {
            line: line[..cut].to_string(),
        }
    }
}
