//! Reading Claude Code's `stream-json` output.
//!
//! One line of NDJSON in, zero or more normalized events out. The parser is
//! deliberately separate from the process that produces the lines: the shapes
//! change without notice, and a fixture is the only thing that says *which*
//! line stopped being understood (roadmap R6).

use serde_json::Value;
use std::collections::HashMap;

use crate::driver::{ActivityItem, AgentEvent, DriverError, TurnOutcome};
use crate::usage::TokenTotals;

/// Reads a session's lines, holding the little state pairing needs.
#[derive(Debug, Default)]
pub struct ClaudeStream {
    session_id: Option<String>,
    /// Calls waiting for their result, so a result can be rendered as the row
    /// it belongs to rather than as an orphan with no title.
    pending: HashMap<String, ActivityItem>,
}

impl ClaudeStream {
    /// The provider's session id, once it has introduced itself. This is what
    /// a resume is built from.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

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
            events.push(AgentEvent::Commands(
                commands
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ));
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

        let mut events = Vec::new();
        for block in content {
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        events.push(AgentEvent::TextDelta(text.to_string()));
                    }
                }
                "thinking" => {
                    if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                        events.push(AgentEvent::Reasoning(text.to_string()));
                    }
                }
                // Encrypted reasoning: the agent thought, and we are told only
                // that. Showing nothing at all would misrepresent the turn.
                "redacted_thinking" => events.push(AgentEvent::Reasoning(String::new())),
                "tool_use" => {
                    let id = string_at(block, "id");
                    let activity = ActivityItem::from_tool(
                        id.clone(),
                        block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        block.get("input").unwrap_or(&Value::Null),
                    );
                    if let Some(id) = id {
                        self.pending.insert(id, activity.clone());
                    }
                    events.push(AgentEvent::ToolCall(activity));
                }
                other => events.push(AgentEvent::Unsupported {
                    shape: format!("assistant.{other}"),
                }),
            }
        }

        if let Some(usage) = usage_of(body) {
            events.push(AgentEvent::Usage(usage));
        }
        Ok(events)
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
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = string_at(block, "tool_use_id");
            // A resumed session replays results whose calls happened before we
            // attached, so an unmatched result still becomes a row.
            let mut activity = id
                .as_ref()
                .and_then(|id| self.pending.remove(id))
                .unwrap_or_else(|| ActivityItem::from_tool(id.clone(), "tool", &Value::Null));

            activity.complete_with(
                &flatten_content(block.get("content")),
                block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            events.push(AgentEvent::ToolResult(activity));
        }
        Ok(events)
    }

    fn result(&mut self, message: &Value) -> Vec<AgentEvent> {
        // Whatever was still outstanding will never be answered now.
        self.pending.clear();

        let mut events = Vec::new();
        if let Some(usage) = usage_of(message) {
            events.push(AgentEvent::Usage(usage));
        }

        let failed = message
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = string_at(message, "result");
        events.push(AgentEvent::SessionResult {
            text: text.clone(),
            failed,
        });

        let subtype = string_at(message, "subtype").unwrap_or_default();
        let outcome = if !failed && subtype != "error" {
            TurnOutcome::Completed
        } else {
            // The subtype is the machine-readable half and the text the
            // human-readable one; a report needs both to be useful.
            let reason = match text {
                Some(text) if !text.is_empty() => format!("{subtype}: {text}"),
                _ => subtype,
            };
            TurnOutcome::Failed { reason }
        };
        events.push(AgentEvent::TurnEnd { outcome });
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
            "message_start" => vec![AgentEvent::TurnStarted],
            "content_block_delta" => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text_delta" => string_at(delta, "text")
                        .map(AgentEvent::TextDelta)
                        .into_iter()
                        .collect(),
                    "thinking_delta" => string_at(delta, "thinking")
                        .map(AgentEvent::Reasoning)
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

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn usage_of(value: &Value) -> Option<TokenTotals> {
    let usage = value.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(TokenTotals {
        input: count("input_tokens"),
        output: count("output_tokens"),
        cache_read: count("cache_read_input_tokens"),
        cache_write: count("cache_creation_input_tokens"),
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
