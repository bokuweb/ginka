//! Reading Claude Code's `stream-json` output.
//!
//! One line of NDJSON in, zero or more normalized events out. The parser is
//! deliberately separate from the process that produces the lines: the shapes
//! change without notice, and a fixture is the only thing that says *which*
//! line stopped being understood (roadmap R6).

use serde_json::Value;
use std::collections::HashMap;

use crate::driver::{ActivityItem, AgentEvent, DriverError};
use ginka_protocol::event::Usage;
use ginka_protocol::model::{PlanUsage, PlanWindow, SessionState};
use ginka_protocol::{SubagentStep, SubagentStepKind, SubagentStepStatus};

/// Reads a session's lines, holding the little state pairing needs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ClaudeStream {
    session_id: Option<String>,
    /// Turns completed on this stream, which is what a checkpoint is filed
    /// against.
    turn: u32,
    /// Set once a partial text delta has arrived. The CLI sends the same text
    /// again in the completed message, so after this the whole-message text is
    /// dropped — otherwise every reply would be recorded twice.
    streaming: bool,
    /// Calls waiting for their result, so a result can be rendered as the row
    /// it belongs to rather than as an orphan with no title.
    pending: HashMap<String, ActivityItem>,
    /// Delegated-agent calls waiting for the report returned to their parent.
    subagents: HashMap<String, String>,
    /// Tool steps inside delegated runs, keyed by the child's tool id.
    subagent_tools: HashMap<String, String>,
    /// Set once this turn has reported hitting a rate limit: the CLI says it
    /// in the assistant's text and again in the result, and one wall is one
    /// reading.
    limit_reported: bool,
}

impl ClaudeStream {
    /// The provider's session id, once it has introduced itself. This is what
    /// a resume is built from.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Read one line of the CLI's stream-json output into normalized events. A
    /// blank line yields none; a line that is not JSON, or misses a field this
    /// build relies on, is an error.
    pub fn push_line(&mut self, line: &str) -> Result<Vec<AgentEvent>, DriverError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(Vec::new());
        }
        let message: Value =
            serde_json::from_str(line).map_err(|_| DriverError::malformed(line))?;

        if let Some(id) = message.get("session_id").and_then(Value::as_str) {
            self.session_id.get_or_insert_with(|| id.to_string());
        }

        match message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "system" => Ok(self.system(&message)),
            "assistant" => self.assistant(&message),
            "user" => self.user(&message),
            "result" => Ok(self.result(&message)),
            "stream_event" => Ok(self.stream_event(&message)),
            other => Ok(vec![AgentEvent::Unsupported {
                shape: other.to_string(),
            }]),
        }
    }

    fn system(&mut self, message: &Value) -> Vec<AgentEvent> {
        let mut events = vec![AgentEvent::Connected {
            session_id: self.session_id.clone(),
            model: string_at(message, "model"),
        }];
        if let Some(commands) = message.get("slash_commands").and_then(Value::as_array) {
            events.push(AgentEvent::Commands {
                commands: commands
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            });
        }
        events
    }

    fn assistant(&mut self, message: &Value) -> Result<Vec<AgentEvent>, DriverError> {
        let body = message.get("message").unwrap_or(&Value::Null);
        let content =
            body.get("content")
                .and_then(Value::as_array)
                .ok_or(DriverError::MissingField {
                    shape: "assistant".into(),
                    field: "content",
                })?;

        if let Some(parent_id) = string_at(message, "parent_tool_use_id") {
            return Ok(self.subagent_assistant(&parent_id, body, content));
        }

        let mut events = Vec::new();
        for block in content {
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                // Already delivered as deltas; the completed message repeats
                // it verbatim.
                "text" if self.streaming => {}
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        events.push(AgentEvent::TextDelta {
                            text: text.to_string(),
                        });
                        if let Some(usage) = self.limit_in(text) {
                            events.push(AgentEvent::PlanUsage { usage });
                        }
                    }
                }
                "thinking" => {
                    if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                        events.push(AgentEvent::Reasoning {
                            text: text.to_string(),
                        });
                    }
                }
                // Encrypted reasoning: the agent thought, and we are told only
                // that. Showing nothing at all would misrepresent the turn.
                "redacted_thinking" => events.push(AgentEvent::Reasoning {
                    text: String::new(),
                }),
                "tool_use" => {
                    let id = string_at(block, "id");
                    let tool = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let activity = ActivityItem::from_tool(
                        id.clone(),
                        tool,
                        block.get("input").unwrap_or(&Value::Null),
                    );
                    if is_subagent_tool(tool)
                        && let Some(id) = id
                    {
                        self.subagents.insert(id.clone(), activity.title.clone());
                        events.push(AgentEvent::SubagentStarted {
                            id,
                            title: activity.title,
                        });
                        continue;
                    }
                    if let Some(id) = id {
                        self.pending.insert(id, activity.clone());
                    }
                    events.push(AgentEvent::ToolCall { activity });
                }
                other => events.push(AgentEvent::Unsupported {
                    shape: format!("assistant.{other}"),
                }),
            }
        }

        if let Some(usage) = usage_of(body) {
            events.push(AgentEvent::Usage { usage });
        }
        Ok(events)
    }

    /// Normalize a child message beneath the delegated run that owns it.
    fn subagent_assistant(
        &mut self,
        parent_id: &str,
        body: &Value,
        content: &[Value],
    ) -> Vec<AgentEvent> {
        // A resumed or truncated stream can expose a child message without
        // the Agent call that owns it. Promoting that private exchange into
        // the parent transcript would both lose its hierarchy and leak noise.
        if !self.subagents.contains_key(parent_id) {
            return Vec::new();
        }
        let message_id = string_at(body, "id").unwrap_or_else(|| parent_id.to_string());
        let mut events = Vec::new();
        for block in content {
            let kind = block.get("type").and_then(Value::as_str);
            let (step_kind, field) = match kind {
                Some("thinking") => (SubagentStepKind::Reasoning, "thinking"),
                Some("text") => (SubagentStepKind::Message, "text"),
                Some("tool_use") => {
                    let Some(id) = string_at(block, "id") else {
                        continue;
                    };
                    let tool = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let activity = ActivityItem::from_tool(
                        Some(id.clone()),
                        tool,
                        block.get("input").unwrap_or(&Value::Null),
                    );
                    self.subagent_tools
                        .insert(id.clone(), parent_id.to_string());
                    events.push(AgentEvent::SubagentStep {
                        parent_id: parent_id.to_string(),
                        step: SubagentStep::new(id, SubagentStepKind::Tool, activity.title)
                            .with_status(SubagentStepStatus::Running),
                    });
                    continue;
                }
                _ => continue,
            };
            let Some(text) = block.get(field).and_then(Value::as_str) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            events.push(AgentEvent::SubagentStep {
                parent_id: parent_id.to_string(),
                step: SubagentStep::new(format!("{message_id}:{field}"), step_kind, text),
            });
        }
        if let Some(usage) = usage_of(body) {
            events.push(AgentEvent::Usage { usage });
        }
        events
    }

    fn user(&mut self, message: &Value) -> Result<Vec<AgentEvent>, DriverError> {
        let content = message
            .get("message")
            .and_then(|body| body.get("content"))
            .and_then(Value::as_array)
            .ok_or(DriverError::MissingField {
                shape: "user".into(),
                field: "content",
            })?;

        let mut events = Vec::new();
        let parent_id = string_at(message, "parent_tool_use_id");
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = string_at(block, "tool_use_id");
            let failed = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(id) = id.as_ref()
                && let Some(owner) = self.subagent_tools.remove(id)
            {
                events.push(AgentEvent::SubagentStep {
                    parent_id: parent_id.clone().unwrap_or(owner),
                    step: SubagentStep::new(id, SubagentStepKind::Tool, "").with_status(
                        if failed {
                            SubagentStepStatus::Failed
                        } else {
                            SubagentStepStatus::Completed
                        },
                    ),
                });
                continue;
            }
            if let Some(id) = id.as_ref()
                && self.subagents.remove(id).is_some()
            {
                let summary = bounded_detail(&flatten_content(block.get("content")));
                events.push(AgentEvent::SubagentFinished {
                    id: id.clone(),
                    summary: (!summary.is_empty()).then_some(summary),
                    failed,
                });
                continue;
            }
            // A resumed session replays results whose calls happened before we
            // attached, so an unmatched result still becomes a row.
            let mut activity = id
                .as_ref()
                .and_then(|id| self.pending.remove(id))
                .unwrap_or_else(|| ActivityItem::from_tool(id.clone(), "tool", &Value::Null));

            if let Some(image) = image_data_url(block.get("content")) {
                // Kept whole only until the daemon transcript boundary turns
                // it into a content-addressed reference. Applying the normal
                // tool-detail bound here would corrupt the base64 first.
                activity.detail = Some(image);
                activity.failed = failed;
                activity.complete = true;
            } else {
                activity.complete_with(&flatten_content(block.get("content")), failed);
            }
            events.push(AgentEvent::ToolResult { activity });
        }
        Ok(events)
    }

    fn result(&mut self, message: &Value) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        if let Some(usage) = usage_of(message) {
            events.push(AgentEvent::Usage { usage });
        }

        let failed = message
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Whatever was still outstanding will never be answered now. Settle
        // delegated rows explicitly before the turn boundary so replay never
        // leaves a child claiming it is still working after its process ended.
        let mut unfinished: Vec<_> = self.subagents.drain().map(|(id, _)| id).collect();
        unfinished.sort();
        events.extend(
            unfinished
                .into_iter()
                .map(|id| AgentEvent::SubagentFinished {
                    id,
                    summary: None,
                    failed,
                }),
        );
        self.pending.clear();
        self.subagent_tools.clear();

        let text = string_at(message, "result");
        let subtype = string_at(message, "subtype").unwrap_or_default();

        // A refused turn is the one thing the CLI says about its windows
        // headless (`docs/accounts.md` §6): the wall, and when it opens.
        if failed && let Some(usage) = text.as_deref().and_then(|text| self.limit_in(text)) {
            events.push(AgentEvent::PlanUsage { usage });
        }
        self.limit_reported = false;

        // A result line is the end of a turn, and the turn number is what a
        // checkpoint is filed against.
        self.turn += 1;
        events.push(AgentEvent::TurnEnd { turn: self.turn });
        events.push(AgentEvent::SessionResult {
            // A result line is the end of the CLI's run, not just of a turn:
            // the whole task it was given is done.
            state: if failed || subtype == "error" {
                SessionState::Failed
            } else {
                SessionState::Finished
            },
            // The subtype is the machine-readable half and the text the
            // human-readable one; a summary is more useful with both.
            summary: match (text, subtype.as_str()) {
                (Some(text), _) if !text.is_empty() => Some(text),
                (_, "") => None,
                (_, subtype) => Some(subtype.to_string()),
            },
        });
        events
    }

    /// Partial deltas, emitted only when the CLI is asked for them. The same
    /// text arrives again in the completed message, so a consumer that renders
    /// both has to treat these as provisional.
    fn stream_event(&mut self, message: &Value) -> Vec<AgentEvent> {
        let event = message.get("event").unwrap_or(&Value::Null);
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            // The supervisor records one provider-neutral `TurnStarted`
            // before it reads output. This vendor marker carries no options
            // and would otherwise overwrite that provenance with blanks.
            "message_start" => Vec::new(),
            "content_block_delta" => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text_delta" => {
                        self.streaming = true;
                        string_at(delta, "text")
                            .map(|text| AgentEvent::TextDelta { text })
                            .into_iter()
                            .collect()
                    }
                    "thinking_delta" => string_at(delta, "thinking")
                        .map(|text| AgentEvent::Reasoning { text })
                        .into_iter()
                        .collect(),
                    // Tool arguments stream in as fragments; the call is
                    // emitted whole from the completed message instead.
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }
}

impl ClaudeStream {
    /// The reading a rate-limit refusal amounts to, once per turn.
    fn limit_in(&mut self, text: &str) -> Option<PlanUsage> {
        if self.limit_reported {
            return None;
        }
        let usage = limit_reached(text)?;
        self.limit_reported = true;
        Some(usage)
    }
}

/// What a refused turn says about the account's windows.
///
/// Claude Code 1.0.124 reports no percentage headless. What reaches a client
/// is the refusal's text: `Claude AI usage limit reached|<unix seconds>` in
/// the older wording, or a sentence naming the window — `5-hour limit
/// reached`, `Weekly limit reached`, `Opus weekly limit reached` — with a
/// reset time in prose the newer one. Either way the window is at the wall,
/// which is a reading of 100 %, and the reset is kept only when it was given
/// as a number: a guess would be worse than silence.
fn limit_reached(text: &str) -> Option<PlanUsage> {
    let lowered = text.to_ascii_lowercase();
    if !(lowered.contains("limit reached") || lowered.contains("usage limit")) {
        return None;
    }
    let label = if lowered.contains("opus") {
        "opus week"
    } else if lowered.contains("weekly") {
        "week"
    } else if lowered.contains("5-hour") {
        "5h"
    } else {
        "limit"
    };
    let resets_at = text
        .rsplit_once('|')
        .and_then(|(_, tail)| tail.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds > 0);
    Some(PlanUsage {
        plan: None,
        windows: vec![PlanWindow {
            label: label.to_string(),
            used_percent: 100.0,
            resets_at,
        }],
    })
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Names Claude Code uses for a delegated-agent call.
fn is_subagent_tool(tool: &str) -> bool {
    matches!(
        tool.rsplit_once("__").map_or(tool, |(_, name)| name),
        "Agent" | "Task"
    )
}

/// Bound a final delegated-agent report by the same rule as ordinary tools.
fn bounded_detail(text: &str) -> String {
    let mut activity = ActivityItem::from_tool(None, "agent", &Value::Null);
    activity.complete_with(text, false);
    activity.detail.unwrap_or_default()
}

fn usage_of(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
        // Cache writes are input the vendor charged differently, and the wire
        // shape keeps one cache figure; folding them in reports what was read
        // rather than dropping it.
        cache_read_tokens: count("cache_read_input_tokens") + count("cache_creation_input_tokens"),
        reasoning_tokens: 0,
        cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
    })
}

/// Tool output arrives as a string or as a list of blocks, depending on the
/// tool. A reader wants text either way.
fn flatten_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Normalize Claude's image result block into the source shape the blob store
/// accepts at the transcript boundary.
fn image_data_url(content: Option<&Value>) -> Option<String> {
    let [block] = content?.as_array()?.as_slice() else {
        return None;
    };
    if block.get("type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let source = block.get("source")?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let media_type = source.get("media_type").and_then(Value::as_str)?;
    let data = source.get("data").and_then(Value::as_str)?;
    if !media_type.starts_with("image/") || data.is_empty() {
        return None;
    }
    Some(format!("data:{media_type};base64,{data}"))
}
