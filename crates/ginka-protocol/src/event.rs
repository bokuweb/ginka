//! The two event streams: what a driver emits, and what the daemon pushes.
//!
//! [`AgentEvent`] is the normalization boundary. Every driver — Claude Code's
//! stream-json, Codex, an ACP peer — turns its vendor shape into these
//! variants, and nothing above `ginka-core::driver` knows which vendor it is
//! talking to (`AGENTS.md` rule 6).

use crate::ids::{ProjectName, SessionId, TerminalId, WorkspaceId};
use crate::model::{
    BranchStatus, ConnectorState, PlanSnapshot, PlanUsage, Session, SessionState, TranscriptEntry,
};
use serde::{Deserialize, Serialize};

/// One normalized thing an agent did.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The session is live. Carries the provider's own session id, which is
    /// what a resume is built from.
    Connected {
        session_id: Option<String>,
        model: Option<String>,
    },
    /// Slash commands the provider defines for itself, merged with the ones on
    /// disk by the composer.
    Commands { commands: Vec<String> },
    /// A turn began. Drivers that cannot tell emit nothing here.
    TurnStarted,
    /// A fragment of assistant text. Deltas are emitted as they arrive and are
    /// never buffered into whole messages by the driver — the UI folds them,
    /// because it is the only layer that knows what is on screen.
    TextDelta { text: String },
    /// A fragment of the agent's reasoning, where the vendor exposes it.
    Reasoning { text: String },
    /// The agent invoked a tool, normalized into the one shape a transcript
    /// renders. `id` on the item correlates with the matching result.
    ToolCall { activity: ActivityItem },
    /// The result of a tool call, completing the call it belongs to.
    ToolResult { activity: ActivityItem },
    /// The agent is blocked on the user. `options` is empty for a free-text
    /// question.
    AskUser {
        id: String,
        question: String,
        options: Vec<String>,
    },
    /// The agent proposed a plan and wants it approved before acting.
    PlanProposal { id: String, plan: String },
    /// The agent wants to do something its access mode does not allow.
    Permission {
        /// Correlates the decision with the paused transport request.
        #[serde(default)]
        id: String,
        /// A human-readable description of the operation awaiting approval.
        request: String,
    },
    /// A steered message reached the running turn.
    SteerAccepted,
    /// It did not, and the caller has to fall back to the queue.
    SteerRejected { reason: Option<String> },
    /// The provider's own name for this session.
    AgentTitle { title: String },
    /// Token and cost accounting for the turn so far.
    Usage { usage: Usage },
    /// The account's rate-limit windows, where the vendor reports them as it
    /// works (`docs/accounts.md` §6).
    PlanUsage { usage: PlanUsage },
    /// A turn boundary. Checkpoints are taken here.
    TurnEnd { turn: u32 },
    /// The session reached a terminal state; no further events will arrive.
    SessionResult {
        state: SessionState,
        summary: Option<String>,
    },
    /// The agent's process is gone.
    ProcessExited { code: Option<i32> },
    /// A shape this build does not understand.
    ///
    /// Loud but not fatal. A vendor adding a message type should appear in the
    /// transcript as "not understood" rather than end the session — and rather
    /// than be silently mis-parsed into something it is not (roadmap R6).
    Unsupported { shape: String },
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
    SessionStarted { session: Box<Session> },
    /// A conversation's provider options changed for its later turns.
    SessionOptionsChanged { session: Box<Session> },
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
    /// A terminal printed something.
    ///
    /// The bytes as the shell wrote them, escapes and all: what they mean is
    /// the client's business, because it is the one with a screen.
    TerminalOutput { terminal: TerminalId, data: String },
    /// A terminal's shell exited, so there is nothing left to type into.
    TerminalClosed { terminal: TerminalId },
    /// An account's rate-limit windows were read again.
    PlanUsageChanged { snapshot: PlanSnapshot },
    /// An account was added, removed, or signed in. Clients re-read the list.
    AccountsChanged,
    /// A chat connector connected, dropped, or failed. The whole state
    /// travels so a window shows the dot going red without asking.
    ConnectorStateChanged { state: ConnectorState },
    /// A commit message asked for with `generate_commit_message` is ready, or
    /// could not be written. Pushed rather than answered because generation
    /// runs a model for tens of seconds, and a request that long would hold
    /// every other client's turn.
    CommitMessageGenerated {
        workspace: WorkspaceId,
        /// Subject, blank line, body — as git takes it. `None` when it
        /// failed, and `error` says why.
        message: Option<String>,
        error: Option<String>,
    },

    /// The daemon is shutting down. Clients should stop reconnecting.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_and_its_result_are_correlated_by_id() {
        let activity = ActivityItem::from_tool(
            Some("call_1".into()),
            "read_file",
            &serde_json::json!({ "path": "a.rs" }),
        );
        let mut finished = activity.clone();
        finished.complete_with("fn main() {}", false);

        let call = AgentEvent::ToolCall { activity };
        let result = AgentEvent::ToolResult { activity: finished };
        let (AgentEvent::ToolCall { activity: call }, AgentEvent::ToolResult { activity: result }) =
            (&call, &result)
        else {
            unreachable!("constructed above")
        };
        assert_eq!(call.id, result.id);
        // The result completes the call rather than renaming it.
        assert_eq!(call.title, result.title);
        assert!(!call.complete && result.complete);
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

// ---------------------------------------------------------------------------

/// What kind of work a tool call is, as far as a reader cares.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// The agent thinking out loud.
    Reasoning,
    /// A command in a shell.
    Command,
    /// A change to a file.
    FileChange,
    /// Looking for something.
    Search,
    /// A plan or todo list.
    Plan,
    /// Anything else the agent can call.
    Tool,
}

/// One tool call, and its result once it arrives.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
    /// The provider's own id for the call, which is how a result finds the
    /// call it belongs to. `None` where a provider does not give one.
    pub id: Option<String>,
    pub kind: ActivityKind,
    /// One line, always non-empty: this is the row a reader scans.
    pub title: String,
    /// The result, once there is one.
    pub detail: Option<String>,
    pub failed: bool,
    pub complete: bool,
}

impl ActivityKind {
    /// A short word for the kind, for the surfaces that print one.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "thinking",
            Self::Command => "run",
            Self::FileChange => "edit",
            Self::Search => "search",
            Self::Plan => "plan",
            Self::Tool => "tool",
        }
    }
}

impl ActivityItem {
    /// The kind as a word, for callers that only want to print it.
    pub fn kind_str(&self) -> &'static str {
        self.kind.as_str()
    }

    /// How much of a tool's output is kept. Enough to see what happened,
    /// bounded so one `yes` cannot own the transcript or the database.
    pub const MAX_DETAIL_BYTES: usize = 16 * 1024;

    /// Normalize a call from the tool's name and its arguments.
    pub fn from_tool(id: Option<String>, tool: &str, input: &serde_json::Value) -> Self {
        let kind = kind_of(tool);
        Self {
            id,
            kind,
            title: title_of(kind, tool, input),
            detail: None,
            failed: false,
            complete: false,
        }
    }

    /// A free-form activity that is not a tool call — reasoning, mostly.
    pub fn note(kind: ActivityKind, title: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            title: single_line(&title.into()),
            detail: None,
            failed: false,
            complete: true,
        }
    }

    /// Attach the result. The title is never rewritten: a row that renames
    /// itself when its result lands is a row nobody can follow.
    pub fn complete_with(&mut self, output: &str, failed: bool) {
        self.detail = Some(truncate(output, Self::MAX_DETAIL_BYTES));
        self.failed = failed;
        self.complete = true;
    }
}

fn kind_of(tool: &str) -> ActivityKind {
    match base_name(tool).to_ascii_lowercase().as_str() {
        "bash" | "shell" | "run" | "exec" | "terminal" => ActivityKind::Command,
        "edit" | "write" | "multiedit" | "apply_patch" | "applypatch" | "str_replace" => {
            ActivityKind::FileChange
        }
        "grep" | "glob" | "search" | "find" | "codebase_search" => ActivityKind::Search,
        "todowrite" | "todo_write" | "plan" | "update_plan" | "exit_plan_mode" => {
            ActivityKind::Plan
        }
        _ => ActivityKind::Tool,
    }
}

/// The first argument that says something a reader wants.
///
/// A name the agent gave the call always wins. After that the order depends on
/// the kind, because the interesting argument does: a search is about its
/// pattern and only incidentally about the directory it ran in, while a file
/// tool is the other way round.
fn title_of(kind: ActivityKind, tool: &str, input: &serde_json::Value) -> String {
    let subject: &[&str] = match kind {
        ActivityKind::Command => &["command"],
        ActivityKind::Search => &["pattern", "query", "regex", "path"],
        ActivityKind::FileChange => &["file_path", "path", "notebook_path"],
        _ => &["file_path", "path", "query", "pattern", "url", "command"],
    };
    for key in ["title", "description"].iter().chain(subject) {
        if let Some(text) = input.get(key).and_then(serde_json::Value::as_str) {
            let line = single_line(text);
            if !line.is_empty() {
                return line;
            }
        }
    }
    let readable = humanize(base_name(tool));
    if readable.is_empty() {
        "Tool call".to_string()
    } else {
        readable
    }
}

/// MCP tools arrive namespaced (`mcp__server__do_thing`); the last segment is
/// the part that names the work.
fn base_name(tool: &str) -> &str {
    tool.rsplit("__").next().unwrap_or(tool)
}

/// `WebFetch` and `create_issue` both become "Web fetch"-shaped prose.
fn humanize(tool: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for part in tool.split(['_', '-']).filter(|part| !part.is_empty()) {
        let mut current = String::new();
        for character in part.chars() {
            if character.is_uppercase() && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(character);
        }
        if !current.is_empty() {
            words.push(current);
        }
    }
    let mut sentence = words.join(" ").to_ascii_lowercase();
    if let Some(first) = sentence.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    sentence
}

/// Titles are one row high. Whatever the argument contained, this is what has
/// to fit on that row.
fn single_line(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… truncated", &text[..cut])
}
