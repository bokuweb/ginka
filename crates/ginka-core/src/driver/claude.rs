//! The Claude Code driver.
//!
//! Runs `claude --print --output-format stream-json`, which writes one JSON
//! object per line. The shapes handled here are pinned by the fixtures in the
//! tests below; a vendor that changes them makes those fail rather than making
//! a live session quietly lose its transcript (`docs/roadmap.md` §7 R6).

use super::{AgentDriver, CommandSpec, ParseState, ProviderModel, SessionSpec};
use ginka_protocol::model::SessionState;
use ginka_protocol::{AgentEvent, Usage};
use serde_json::Value;

/// The Claude Code CLI.
#[derive(Debug, Clone)]
pub struct ClaudeDriver {
    program: String,
    env: Vec<(String, String)>,
}

impl Default for ClaudeDriver {
    fn default() -> Self {
        Self::with_program("claude")
    }
}

impl ClaudeDriver {
    /// A driver that runs `program` instead of whatever is on `PATH`.
    ///
    /// Tests point this at the fake agent; a user with a version-managed
    /// install points it at their own binary.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            env: Vec::new(),
        }
    }

    /// Set an environment variable for every process this driver starts.
    ///
    /// A gateway's base URL, a version-managed install's `PATH`, or — in the
    /// tests — the script the fake agent should read.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The flags every invocation needs.
    ///
    /// `--verbose` is not decoration: without it the CLI collapses
    /// `stream-json` down to the final result, and the transcript would arrive
    /// as one block at the end of the turn.
    fn streaming_args(&self, spec: &SessionSpec) -> Vec<String> {
        let mut args = vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--include-partial-messages".to_string(),
            "--verbose".to_string(),
        ];
        if let Some(model) = &spec.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args
    }
}

impl AgentDriver for ClaudeDriver {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn models(&self) -> Vec<ProviderModel> {
        // Aliases rather than dated ids: the CLI resolves them, so this list
        // does not go stale every time a model ships.
        ["opus", "sonnet", "haiku"]
            .into_iter()
            .map(|id| ProviderModel {
                id: id.to_string(),
                label: id.to_string(),
            })
            .collect()
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec::new(&self.program).arg("--version")
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program).args(self.streaming_args(spec));
        command.args.push(spec.prompt.clone());
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program)
            .args(self.streaming_args(spec))
            .arg("--resume")
            .arg(vendor_session_id);
        command.args.push(spec.prompt.clone());
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            state.unrecognized += 1;
            return Vec::new();
        };

        if let Some(id) = value.get("session_id").and_then(Value::as_str) {
            state.vendor_session_id = Some(id.to_string());
        }

        match value.get("type").and_then(Value::as_str) {
            Some("system") => {
                state.recognized += 1;
                Vec::new()
            }
            Some("stream_event") => {
                state.recognized += 1;
                parse_stream_event(&value, state)
            }
            Some("assistant") => {
                state.recognized += 1;
                parse_assistant(&value, state)
            }
            Some("user") => {
                state.recognized += 1;
                parse_tool_results(&value)
            }
            Some("result") => {
                state.recognized += 1;
                parse_result(&value, state)
            }
            _ => {
                state.unrecognized += 1;
                Vec::new()
            }
        }
    }
}

/// Partial message chunks, which arrive when `--include-partial-messages` is on.
fn parse_stream_event(value: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let Some(event) = value.get("event") else {
        return Vec::new();
    };
    let Some(delta) = event.get("delta") else {
        return Vec::new();
    };
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => {
            // From here on the whole message will also arrive; ignoring it is
            // what keeps every reply from being recorded twice.
            state.streaming = true;
            text(delta.get("text")).map(|text| vec![AgentEvent::TextDelta { text }])
        }
        Some("thinking_delta") => {
            state.streaming = true;
            text(delta.get("thinking")).map(|text| vec![AgentEvent::Reasoning { text }])
        }
        _ => None,
    }
    .unwrap_or_default()
}

/// A whole assistant message: text, reasoning and tool calls.
fn parse_assistant(value: &Value, state: &ParseState) -> Vec<AgentEvent> {
    let Some(content) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            // Already delivered as deltas.
            Some("text") if state.streaming => {}
            Some("text") => {
                if let Some(text) = text(block.get("text")) {
                    events.push(AgentEvent::TextDelta { text });
                }
            }
            Some("thinking") if state.streaming => {}
            Some("thinking") => {
                if let Some(text) = text(block.get("thinking")) {
                    events.push(AgentEvent::Reasoning { text });
                }
            }
            Some("tool_use") => {
                events.push(AgentEvent::ToolCall {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    input: block.get("input").cloned().unwrap_or(Value::Null),
                });
            }
            _ => {}
        }
    }
    events
}

/// Tool results, which the CLI reports as a user message.
fn parse_tool_results(value: &Value) -> Vec<AgentEvent> {
    let Some(content) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| AgentEvent::ToolResult {
            id: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            output: flatten_content(block.get("content")),
            is_error: block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

/// The end of a turn: accounting, the boundary, and how it went.
fn parse_result(value: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    if let Some(usage) = value.get("usage") {
        events.push(AgentEvent::Usage {
            usage: Usage {
                input_tokens: number(usage.get("input_tokens")),
                output_tokens: number(usage.get("output_tokens")),
                cache_read_tokens: number(usage.get("cache_read_input_tokens")),
                reasoning_tokens: 0,
                cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
            },
        });
    }

    state.turn += 1;
    events.push(AgentEvent::TurnEnd { turn: state.turn });

    let failed = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("subtype")
            .and_then(Value::as_str)
            .is_some_and(|subtype| subtype != "success");
    events.push(AgentEvent::SessionResult {
        state: if failed {
            SessionState::Failed
        } else {
            SessionState::Finished
        },
        summary: value
            .get("result")
            .and_then(Value::as_str)
            .map(summarize)
            .filter(|summary| !summary.is_empty()),
    });
    events
}

/// A non-empty string from a JSON field.
fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

fn number(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

/// Tool output arrives either as a string or as a list of content blocks.
fn flatten_content(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// The first line of the agent's answer, for the sidebar.
fn summarize(result: &str) -> String {
    let first = result.lines().next().unwrap_or_default().trim();
    if first.chars().count() <= 120 {
        return first.to_string();
    }
    let truncated: String = first.chars().take(119).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[&str]) -> (Vec<AgentEvent>, ParseState) {
        let driver = ClaudeDriver::default();
        let mut state = ParseState::default();
        let mut events = Vec::new();
        for line in lines {
            events.extend(driver.parse_line(line, &mut state));
        }
        (events, state)
    }

    #[test]
    fn the_start_command_asks_for_a_streamed_transcript() {
        let driver = ClaudeDriver::default();
        let command = driver.start_command(
            &SessionSpec::new("/tmp/wt", "write the test first")
                .with_model(Some("opus".to_string())),
        );
        assert_eq!(command.program, "claude");
        assert!(command.args.contains(&"--print".to_string()));
        assert_eq!(
            command
                .args
                .windows(2)
                .find(|pair| pair[0] == "--output-format"),
            Some(["--output-format".to_string(), "stream-json".to_string()].as_slice()),
        );
        // Without --verbose the CLI collapses the stream into one final blob.
        assert!(command.args.contains(&"--verbose".to_string()));
        assert!(command.args.contains(&"opus".to_string()));
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("write the test first"),
            "the prompt goes last, after the flags"
        );
    }

    #[test]
    fn a_drivers_environment_reaches_the_process_it_starts() {
        let driver = ClaudeDriver::default().with_env("ANTHROPIC_BASE_URL", "http://localhost:1");
        let command = driver.start_command(&SessionSpec::new("/tmp/wt", "hello"));
        assert_eq!(
            command.env,
            vec![(
                "ANTHROPIC_BASE_URL".to_string(),
                "http://localhost:1".to_string()
            )]
        );
    }

    #[test]
    fn a_resume_continues_the_vendors_own_session() {
        let driver = ClaudeDriver::default();
        let command =
            driver.resume_command(&SessionSpec::new("/tmp/wt", "and now the fix"), "abc-123");
        assert_eq!(
            command.args.windows(2).find(|pair| pair[0] == "--resume"),
            Some(["--resume".to_string(), "abc-123".to_string()].as_slice()),
        );
    }

    #[test]
    fn the_session_id_is_picked_up_so_the_conversation_can_be_resumed() {
        let (_, state) =
            parse(&[r#"{"type":"system","subtype":"init","session_id":"abc-123","model":"opus"}"#]);
        assert_eq!(state.vendor_session_id.as_deref(), Some("abc-123"));
        assert_eq!(state.recognized, 1);
    }

    #[test]
    fn a_whole_assistant_message_becomes_text_and_tool_calls() {
        let (events, _) = parse(&[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading the file."},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"a.rs"}}]},"session_id":"s"}"#,
        ]);
        assert_eq!(
            events,
            vec![
                AgentEvent::TextDelta {
                    text: "Reading the file.".into()
                },
                AgentEvent::ToolCall {
                    id: "toolu_1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({ "file_path": "a.rs" }),
                },
            ]
        );
    }

    #[test]
    fn thinking_blocks_are_reasoning_not_text() {
        let (events, _) = parse(&[
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"weighing it up"}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![AgentEvent::Reasoning {
                text: "weighing it up".into()
            }]
        );
    }

    #[test]
    fn a_streamed_message_is_not_recorded_twice_when_the_whole_one_follows() {
        // With --include-partial-messages the CLI sends every chunk and then
        // the assembled message. Emitting both would double every reply.
        let (events, state) = parse(&[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#,
        ]);
        assert!(state.streaming);
        assert_eq!(
            events,
            vec![
                AgentEvent::TextDelta { text: "Hel".into() },
                AgentEvent::TextDelta { text: "lo".into() },
            ]
        );
    }

    #[test]
    fn a_tool_call_still_arrives_while_text_is_streaming() {
        // Only the text is duplicated by the assembled message; dropping the
        // whole message wholesale would lose the tool calls in it.
        let (events, _) = parse(&[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"ok"}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                AgentEvent::TextDelta { text: "ok".into() },
                AgentEvent::ToolCall {
                    id: "t1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                },
            ]
        );
    }

    #[test]
    fn tool_results_are_paired_with_the_call_that_asked_for_them() {
        let (events, _) = parse(&[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"fn main() {}","is_error":false}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![AgentEvent::ToolResult {
                id: "toolu_1".into(),
                output: "fn main() {}".into(),
                is_error: false,
            }]
        );
    }

    #[test]
    fn tool_output_sent_as_content_blocks_is_flattened() {
        let (events, _) = parse(&[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"one"},{"type":"text","text":"two"}]}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![AgentEvent::ToolResult {
                id: "t".into(),
                output: "one\ntwo".into(),
                is_error: false,
            }]
        );
    }

    #[test]
    fn a_failing_tool_result_is_marked_as_one() {
        let (events, _) = parse(&[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"no such file","is_error":true}]}}"#,
        ]);
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::ToolResult { is_error: true, .. }]
        ));
    }

    #[test]
    fn the_result_line_closes_the_turn_with_accounting() {
        let (events, state) = parse(&[
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.\nDetails follow.","session_id":"s","total_cost_usd":0.0123,"usage":{"input_tokens":10,"output_tokens":4,"cache_read_input_tokens":7}}"#,
        ]);
        assert_eq!(state.turn, 1);
        assert_eq!(
            events,
            vec![
                AgentEvent::Usage {
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 4,
                        cache_read_tokens: 7,
                        reasoning_tokens: 0,
                        cost_usd: Some(0.0123),
                    }
                },
                AgentEvent::TurnEnd { turn: 1 },
                AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    // One line, for the sidebar; the transcript has the rest.
                    summary: Some("Done.".into()),
                },
            ]
        );
    }

    #[test]
    fn a_result_that_reports_an_error_ends_the_session_as_failed() {
        let (events, _) = parse(&[
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"s"}"#,
        ]);
        assert!(events.contains(&AgentEvent::SessionResult {
            state: SessionState::Failed,
            summary: None,
        }));
    }

    #[test]
    fn a_line_that_is_not_json_is_counted_rather_than_raised() {
        // Vendors print diagnostics on stdout; one stray line is not a failed
        // session.
        let (events, state) = parse(&["Warning: something happened"]);
        assert!(events.is_empty());
        assert_eq!(state.unrecognized, 1);
        assert!(state.understood_nothing());
    }

    #[test]
    fn a_session_with_one_understood_line_has_not_hit_a_format_change() {
        let (_, state) = parse(&[
            "Warning: something happened",
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
        ]);
        assert!(!state.understood_nothing());
    }

    #[test]
    fn blank_lines_are_ignored_entirely() {
        let (events, state) = parse(&["", "   "]);
        assert!(events.is_empty());
        assert_eq!(state.unrecognized, 0, "a blank line is not a format change");
    }
}
