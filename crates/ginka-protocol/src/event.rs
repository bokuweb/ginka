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
    /// A turn began under these effective session options.
    ///
    /// The supervisor emits this before reading vendor output so every driver
    /// has the same provenance. Fields default for transcripts written before
    /// protocol 10; a provider may refine the model later through
    /// [`Connected`](Self::Connected).
    TurnStarted {
        /// Stable provider/driver id, such as `claude` or `codex`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Requested model id, absent when the provider chooses its default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Requested reasoning level for this turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        /// Requested service tier for this turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service_tier: Option<String>,
    },
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
    /// A delegated agent began work under one parent transcript row.
    SubagentStarted {
        /// Provider call id used to correlate its later steps and result.
        id: String,
        /// The brief or description the parent gave the delegated run.
        title: String,
    },
    /// One piece of work performed by a delegated agent.
    SubagentStep {
        /// The [`SubagentStarted`](Self::SubagentStarted) id this belongs to.
        parent_id: String,
        /// The bounded, provider-neutral-and-tool-independent step.
        step: SubagentStep,
    },
    /// A delegated agent returned to its parent.
    SubagentFinished {
        /// The [`SubagentStarted`](Self::SubagentStarted) id this completes.
        id: String,
        /// Its bounded final report, when the provider exposes one.
        summary: Option<String>,
        /// Whether the delegated run failed.
        failed: bool,
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
    /// Current context-window occupancy when the provider reports both sides.
    ContextUsage { usage: ContextUsage },
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

/// One provider-reported context-window reading.
///
/// This is deliberately separate from [`Usage`]: session accounting is
/// cumulative, while this gauge may fall after provider compaction.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// Tokens occupying the model's current context.
    pub used_tokens: u64,
    /// Total context capacity for the model that produced the reading.
    pub window_tokens: u64,
    /// Whether this provider exposes an explicit manual compaction operation.
    #[serde(default)]
    pub can_compact: bool,
}

impl ContextUsage {
    /// Context occupancy as a display percentage, bounded against malformed
    /// or over-capacity vendor readings.
    pub fn used_percent(&self) -> f64 {
        if self.window_tokens == 0 {
            return 0.0;
        }
        (self.used_tokens as f64 * 100.0 / self.window_tokens as f64).clamp(0.0, 100.0)
    }
}

/// The kind of information one delegated-agent step carries.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStepKind {
    /// Reasoning exposed by the provider.
    Reasoning,
    /// Prose addressed to the parent agent.
    Message,
    /// A tool invocation inside the delegated run.
    Tool,
}

/// Lifecycle state for a delegated agent's tool step.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStepStatus {
    /// The tool is still running.
    Running,
    /// The tool completed successfully.
    Completed,
    /// The tool failed.
    Failed,
}

/// One bounded row in a delegated agent's trail.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentStep {
    /// Provider id used to update a running tool in place.
    pub id: String,
    /// Whether this is thought, prose or a tool.
    pub kind: SubagentStepKind,
    /// Human-readable content, bounded by [`Self::MAX_TEXT_CHARS`].
    pub text: String,
    /// Present for a tool step whose lifecycle the provider exposes.
    pub status: Option<SubagentStepStatus>,
}

impl SubagentStep {
    /// Maximum Unicode scalar count kept for one step.
    pub const MAX_TEXT_CHARS: usize = 2_000;

    /// Build a step with bounded text and no lifecycle state.
    pub fn new(id: impl Into<String>, kind: SubagentStepKind, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut chars = text.chars();
        let mut bounded = chars
            .by_ref()
            .take(Self::MAX_TEXT_CHARS)
            .collect::<String>();
        if chars.next().is_some() {
            bounded.push('…');
        }
        Self {
            id: id.into(),
            kind,
            text: bounded,
            status: None,
        }
    }

    /// Attach the lifecycle state supplied by a tool notification.
    pub fn with_status(mut self, status: SubagentStepStatus) -> Self {
        self.status = Some(status);
        self
    }
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
    /// A session's editable follow-up queue changed. Clients re-read the
    /// ordered rows so concurrent edits from another client cannot diverge.
    SessionQueueChanged {
        /// Session whose queue clients should re-read.
        session: SessionId,
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
    fn plan_tools_expose_a_bounded_provider_neutral_task_list() {
        let activity = ActivityItem::from_tool(
            Some("tasks-1".into()),
            "TodoWrite",
            &serde_json::json!({
                "todos": [
                    {"content": "Inspect the parser", "status": "completed"},
                    {"content": "Implement the card", "status": "in_progress"},
                    {"content": "Run the suite", "status": "pending"},
                    {"content": "Discarded approach", "status": "cancelled"}
                ]
            }),
        );

        assert_eq!(
            activity.tasks,
            Some(vec![
                TaskItem::new("Inspect the parser", TaskStatus::Completed),
                TaskItem::new("Implement the card", TaskStatus::InProgress),
                TaskItem::new("Run the suite", TaskStatus::Pending),
                TaskItem::new("Discarded approach", TaskStatus::Cancelled),
            ])
        );
    }

    #[test]
    fn task_snapshots_bound_rows_and_labels_on_unicode_boundaries() {
        let long = "界".repeat(TaskItem::MAX_LABEL_CHARS + 1);
        let todos = (0..=ActivityItem::MAX_TASKS)
            .map(|index| {
                serde_json::json!({
                    "content": if index == 0 { long.as_str() } else { "task" },
                    "status": "pending"
                })
            })
            .collect::<Vec<_>>();
        let activity =
            ActivityItem::from_tool(None, "update_plan", &serde_json::json!({ "plan": todos }));
        let tasks = activity.tasks.expect("the plan has visible work");

        assert_eq!(tasks.len(), ActivityItem::MAX_TASKS);
        assert_eq!(
            tasks[0].label.chars().count(),
            TaskItem::MAX_LABEL_CHARS + 1,
            "the ellipsis follows the bounded label"
        );
        assert!(tasks[0].label.ends_with('…'));
    }

    #[test]
    fn activity_from_protocol_eight_defaults_to_no_task_snapshot() {
        let activity: ActivityItem = serde_json::from_value(serde_json::json!({
            "id": null,
            "kind": "tool",
            "title": "Read",
            "detail": null,
            "failed": false,
            "complete": false
        }))
        .unwrap();

        assert_eq!(activity.tasks, None);
    }

    #[test]
    fn an_old_turn_started_event_defaults_to_unknown_provenance() {
        let event: AgentEvent =
            serde_json::from_value(serde_json::json!({ "kind": "turn_started" })).unwrap();

        assert_eq!(
            event,
            AgentEvent::TurnStarted {
                provider: None,
                model: None,
                reasoning_effort: None,
                service_tier: None,
            }
        );
    }

    #[test]
    fn context_percentage_is_bounded_even_when_a_vendor_reports_overage() {
        let usage = ContextUsage {
            used_tokens: 150,
            window_tokens: 100,
            can_compact: false,
        };

        assert_eq!(usage.used_percent(), 100.0);
    }

    #[test]
    fn a_subagent_step_is_bounded_on_a_character_boundary() {
        let text = "界".repeat(SubagentStep::MAX_TEXT_CHARS + 1);
        let step = SubagentStep::new("step", SubagentStepKind::Message, text);
        assert_eq!(step.text.chars().count(), SubagentStep::MAX_TEXT_CHARS + 1);
        assert!(step.text.ends_with('…'));
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

/// Lifecycle state of one provider-neutral task.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Work has not started.
    Pending,
    /// Work is the provider's current step.
    InProgress,
    /// Work finished successfully.
    Completed,
    /// Work was deliberately skipped or abandoned.
    Cancelled,
}

/// One bounded item from an agent-maintained task list.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    /// Human-readable work description, bounded by [`Self::MAX_LABEL_CHARS`].
    pub label: String,
    /// Current lifecycle state reported by the provider.
    pub status: TaskStatus,
}

impl TaskItem {
    /// Maximum Unicode scalar count retained for one task label.
    pub const MAX_LABEL_CHARS: usize = 500;

    /// Build one task while preserving a valid UTF-8 boundary at the cap.
    pub fn new(label: impl Into<String>, status: TaskStatus) -> Self {
        let label = label.into();
        let mut characters = label.chars();
        let mut bounded = characters
            .by_ref()
            .take(Self::MAX_LABEL_CHARS)
            .collect::<String>();
        if characters.next().is_some() {
            bounded.push('…');
        }
        Self {
            label: bounded,
            status,
        }
    }
}

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
    /// A normalized task-list snapshot for plan/todo tools.
    ///
    /// Absent on ordinary tools and on plan calls whose provider shape does
    /// not expose structured items. The cap prevents an agent-controlled list
    /// from owning an unbounded transcript row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<TaskItem>>,
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

    /// Maximum number of structured task rows retained from one update.
    pub const MAX_TASKS: usize = 100;

    /// Normalize a call from the tool's name and its arguments.
    pub fn from_tool(id: Option<String>, tool: &str, input: &serde_json::Value) -> Self {
        let kind = kind_of(tool);
        Self {
            id,
            kind,
            title: title_of(kind, tool, input),
            tasks: (kind == ActivityKind::Plan)
                .then(|| tasks_from(input))
                .filter(|tasks| !tasks.is_empty()),
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
            tasks: None,
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

fn tasks_from(input: &serde_json::Value) -> Vec<TaskItem> {
    let Some(items) = ["todos", "tasks", "plan"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_array))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let label = ["content", "text", "step", "label", "activeForm"]
                .into_iter()
                .find_map(|key| item.get(key).and_then(serde_json::Value::as_str))?;
            let label = label.trim();
            if label.is_empty() {
                return None;
            }
            let status = match item
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "completed" | "complete" | "done" => TaskStatus::Completed,
                "in_progress" | "in-progress" | "active" | "running" => TaskStatus::InProgress,
                "cancelled" | "canceled" | "skipped" => TaskStatus::Cancelled,
                _ => TaskStatus::Pending,
            };
            Some(TaskItem::new(label, status))
        })
        .take(ActivityItem::MAX_TASKS)
        .collect()
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
