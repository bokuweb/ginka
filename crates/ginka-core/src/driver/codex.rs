//! The Codex driver.
//!
//! Runs `codex exec --json`, which writes one JSON object per line. Two
//! generations of that format are in the wild — a `thread`/`item`/`turn`
//! vocabulary, and an older envelope with the payload under `msg` — and both
//! are handled, because the CLI a user has installed is not ours to choose.
//!
//! As with every driver, the shapes are pinned by the fixtures below rather
//! than by a live session.

use super::{AgentDriver, CommandSpec, ParseState, ProviderModel, SessionSpec};
use ginka_protocol::model::SessionState;
use ginka_protocol::{AgentEvent, Usage};
use serde_json::Value;

/// The Codex CLI.
#[derive(Debug, Clone)]
pub struct CodexDriver {
    program: String,
    env: Vec<(String, String)>,
}

impl Default for CodexDriver {
    fn default() -> Self {
        Self::with_program("codex")
    }
}

impl CodexDriver {
    /// A driver that runs `program` instead of whatever is on `PATH`.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            env: Vec::new(),
        }
    }

    /// Set an environment variable for every process this driver starts.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn model_args(spec: &SessionSpec) -> Vec<String> {
        match &spec.model {
            Some(model) => vec!["--model".to_string(), model.clone()],
            None => Vec::new(),
        }
    }
}

impl AgentDriver for CodexDriver {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn models(&self) -> Vec<ProviderModel> {
        // Left to the CLI's own configuration: Codex resolves the default from
        // `~/.codex/config.toml`, and a hardcoded list here would override a
        // user's choice with a stale one.
        Vec::new()
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec::new(&self.program).arg("--version")
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program)
            .arg("exec")
            .arg("--json")
            .args(Self::model_args(spec));
        command.args.push(spec.prompt.clone());
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program)
            .arg("exec")
            .arg("resume")
            .arg(vendor_session_id)
            .arg("--json")
            .args(Self::model_args(spec));
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

        if let Some(id) = value
            .get("thread_id")
            .or_else(|| value.get("session_id"))
            .or_else(|| value.get("conversation_id"))
            .and_then(Value::as_str)
        {
            state.vendor_session_id = Some(id.to_string());
        }

        // The older format wraps everything in `msg`; unwrap it and treat the
        // inner `type` the same way.
        if let Some(inner) = value.get("msg") {
            return parse_legacy(inner, state);
        }

        match value.get("type").and_then(Value::as_str) {
            Some(
                "thread.started" | "session.created" | "turn.started" | "item.started"
                | "item.updated",
            ) => {
                state.recognized += 1;
                Vec::new()
            }
            Some("item.completed") => {
                state.recognized += 1;
                value.get("item").map(parse_item).unwrap_or_default()
            }
            Some("turn.completed") => {
                state.recognized += 1;
                let mut events = Vec::new();
                if let Some(usage) = value.get("usage") {
                    events.push(AgentEvent::Usage {
                        usage: usage_from(usage),
                    });
                }
                state.turn += 1;
                events.push(AgentEvent::TurnEnd { turn: state.turn });
                events.push(AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    summary: None,
                });
                events
            }
            Some("turn.failed" | "error") => {
                state.recognized += 1;
                vec![AgentEvent::SessionResult {
                    state: SessionState::Failed,
                    summary: value
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .or_else(|| value.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }]
            }
            _ => {
                state.unrecognized += 1;
                Vec::new()
            }
        }
    }
}

/// One completed item of the modern format.
fn parse_item(item: &Value) -> Vec<AgentEvent> {
    let kind = item
        .get("item_type")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("item")
        .to_string();

    match kind {
        "agent_message" => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| {
                vec![AgentEvent::TextDelta {
                    text: text.to_string(),
                }]
            })
            .unwrap_or_default(),
        "reasoning" => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| {
                vec![AgentEvent::Reasoning {
                    text: text.to_string(),
                }]
            })
            .unwrap_or_default(),
        "command_execution" => {
            let command = item
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // Codex reports a command once it has run, so the call and its
            // result arrive together; the pairing the UI draws is still made
            // here rather than left for it to infer.
            vec![
                AgentEvent::ToolCall {
                    id: id.clone(),
                    name: "shell".to_string(),
                    input: serde_json::json!({ "command": command }),
                },
                AgentEvent::ToolResult {
                    id,
                    output: item
                        .get("aggregated_output")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    is_error: item
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0),
                },
            ]
        }
        "file_change" => vec![AgentEvent::ToolCall {
            id,
            name: "edit".to_string(),
            input: item.get("changes").cloned().unwrap_or(Value::Null),
        }],
        _ => Vec::new(),
    }
}

/// The older `{"id":…,"msg":{…}}` envelope.
fn parse_legacy(msg: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    match msg.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            state.recognized += 1;
            msg.get("message")
                .and_then(Value::as_str)
                .map(|text| {
                    vec![AgentEvent::TextDelta {
                        text: text.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        Some("agent_reasoning") => {
            state.recognized += 1;
            msg.get("text")
                .and_then(Value::as_str)
                .map(|text| {
                    vec![AgentEvent::Reasoning {
                        text: text.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        Some("exec_command_begin") => {
            state.recognized += 1;
            vec![AgentEvent::ToolCall {
                id: msg
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call")
                    .to_string(),
                name: "shell".to_string(),
                input: msg.get("command").cloned().unwrap_or(Value::Null),
            }]
        }
        Some("exec_command_end") => {
            state.recognized += 1;
            vec![AgentEvent::ToolResult {
                id: msg
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call")
                    .to_string(),
                output: msg
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                is_error: msg
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0),
            }]
        }
        Some("task_complete") => {
            state.recognized += 1;
            state.turn += 1;
            vec![
                AgentEvent::TurnEnd { turn: state.turn },
                AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    summary: msg
                        .get("last_agent_message")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
            ]
        }
        Some("token_count") => {
            state.recognized += 1;
            vec![AgentEvent::Usage {
                usage: usage_from(msg.get("info").unwrap_or(msg)),
            }]
        }
        _ => {
            state.unrecognized += 1;
            Vec::new()
        }
    }
}

/// Codex's accounting, under whichever names this generation uses.
fn usage_from(usage: &Value) -> Usage {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    Usage {
        input_tokens: number(&["input_tokens", "total_input_tokens"]),
        output_tokens: number(&["output_tokens", "total_output_tokens"]),
        cache_read_tokens: number(&["cached_input_tokens", "cached_tokens"]),
        reasoning_tokens: number(&["reasoning_output_tokens"]),
        // Codex does not price a run, so a cost here would be invented.
        cost_usd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[&str]) -> (Vec<AgentEvent>, ParseState) {
        let driver = CodexDriver::default();
        let mut state = ParseState::default();
        let mut events = Vec::new();
        for line in lines {
            events.extend(driver.parse_line(line, &mut state));
        }
        (events, state)
    }

    #[test]
    fn the_start_command_asks_for_jsonl() {
        let driver = CodexDriver::default();
        let command = driver.start_command(&SessionSpec::new("/tmp/wt", "do the thing"));
        assert_eq!(command.program, "codex");
        assert_eq!(command.args[0], "exec");
        assert!(command.args.contains(&"--json".to_string()));
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("do the thing")
        );
    }

    #[test]
    fn a_resume_names_the_session_before_the_prompt() {
        let driver = CodexDriver::default();
        let command = driver.resume_command(&SessionSpec::new("/tmp/wt", "carry on"), "01H");
        assert_eq!(&command.args[..3], &["exec", "resume", "01H"]);
        assert_eq!(command.args.last().map(String::as_str), Some("carry on"));
    }

    #[test]
    fn the_thread_id_is_what_a_resume_is_built_from() {
        let (_, state) = parse(&[r#"{"type":"thread.started","thread_id":"01H"}"#]);
        assert_eq!(state.vendor_session_id.as_deref(), Some("01H"));
    }

    #[test]
    fn a_completed_agent_message_becomes_text() {
        let (events, _) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_0","item_type":"agent_message","text":"Done."}}"#,
        ]);
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                text: "Done.".into()
            }]
        );
    }

    #[test]
    fn a_command_execution_arrives_as_a_call_and_its_result() {
        let (events, _) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_1","item_type":"command_execution","command":"cargo test","aggregated_output":"ok","exit_code":0}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                AgentEvent::ToolCall {
                    id: "item_1".into(),
                    name: "shell".into(),
                    input: serde_json::json!({ "command": "cargo test" }),
                },
                AgentEvent::ToolResult {
                    id: "item_1".into(),
                    output: "ok".into(),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn a_non_zero_exit_marks_the_tool_result_as_an_error() {
        let (events, _) = parse(&[
            r#"{"type":"item.completed","item":{"id":"i","item_type":"command_execution","command":"false","aggregated_output":"","exit_code":1}}"#,
        ]);
        assert!(matches!(
            events.as_slice(),
            [_, AgentEvent::ToolResult { is_error: true, .. }]
        ));
    }

    #[test]
    fn a_completed_turn_closes_the_session_with_accounting() {
        let (events, state) = parse(&[
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":8}}"#,
        ]);
        assert_eq!(state.turn, 1);
        assert_eq!(
            events,
            vec![
                AgentEvent::Usage {
                    usage: Usage {
                        input_tokens: 100,
                        output_tokens: 8,
                        cache_read_tokens: 20,
                        reasoning_tokens: 0,
                        cost_usd: None,
                    }
                },
                AgentEvent::TurnEnd { turn: 1 },
                AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    summary: None,
                },
            ]
        );
    }

    #[test]
    fn a_failed_turn_carries_the_reason() {
        let (events, _) =
            parse(&[r#"{"type":"turn.failed","error":{"message":"the model refused"}}"#]);
        assert_eq!(
            events,
            vec![AgentEvent::SessionResult {
                state: SessionState::Failed,
                summary: Some("the model refused".into()),
            }]
        );
    }

    #[test]
    fn the_older_msg_envelope_is_understood_too() {
        // A user's installed CLI is not ours to choose, and the older format
        // is still what several versions emit.
        let (events, state) = parse(&[
            r#"{"id":"0","msg":{"type":"agent_message","message":"Hello"}}"#,
            r#"{"id":"1","msg":{"type":"exec_command_begin","call_id":"c1","command":["ls"]}}"#,
            r#"{"id":"2","msg":{"type":"exec_command_end","call_id":"c1","stdout":"a.rs","exit_code":0}}"#,
            r#"{"id":"3","msg":{"type":"task_complete","last_agent_message":"Hello"}}"#,
        ]);
        assert_eq!(state.unrecognized, 0);
        assert_eq!(
            events.first(),
            Some(&AgentEvent::TextDelta {
                text: "Hello".into()
            })
        );
        assert!(matches!(events[1], AgentEvent::ToolCall { .. }));
        assert!(matches!(events[2], AgentEvent::ToolResult { .. }));
        assert_eq!(
            events.last(),
            Some(&AgentEvent::SessionResult {
                state: SessionState::Finished,
                summary: Some("Hello".into()),
            })
        );
    }

    #[test]
    fn an_unknown_line_is_counted_so_a_format_change_is_visible() {
        let (events, state) = parse(&[r#"{"type":"something.new","payload":{}}"#]);
        assert!(events.is_empty());
        assert_eq!(state.unrecognized, 1);
        assert!(state.understood_nothing());
    }
}
