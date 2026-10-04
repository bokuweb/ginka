//! The Agent Client Protocol driver.
//!
//! ACP is JSON-RPC 2.0 over the agent's stdio, and one adapter covers every
//! agent that speaks it (`docs/roadmap.md` §4.2): Gemini CLI and OpenCode are
//! the ones this build names. Unlike the `claude` and `codex` streams it is a
//! conversation rather than a report — the prompt can only be sent once the
//! agent has answered `session/new` with an id — so the driver keeps its half
//! of the exchange in [`AcpState`] and hands the supervisor what to write
//! next through [`ParseState::outbox`].
//!
//! A turn is one process, as with every driver: `initialize`, then
//! `session/new` (or `session/load` for a conversation the agent can reload),
//! then one `session/prompt`, whose answer ends the turn. The agent's replay
//! of a loaded conversation is not recorded again.

use super::{ActivityItem, ActivityKind, AgentDriver, CommandSpec, ParseState, SessionSpec};
use ginka_protocol::AgentEvent;
use ginka_protocol::event::{TaskItem, TaskStatus};
use ginka_protocol::model::SessionState;
use ginka_protocol::provider::{AccessMode, ProviderModel};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The ACP version this driver speaks.
const PROTOCOL_VERSION: u64 = 1;

/// JSON-RPC's "method not found", for requests this client does not serve.
const METHOD_NOT_FOUND: i64 = -32601;

/// One agent reached over ACP.
#[derive(Debug, Clone)]
pub struct AcpDriver {
    id: &'static str,
    display_name: &'static str,
    program: String,
    /// What puts the CLI into ACP mode.
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl AcpDriver {
    /// Gemini CLI, which serves ACP behind a flag.
    pub fn gemini() -> Self {
        Self::new("gemini", "Gemini", "gemini", &["--experimental-acp"])
    }

    /// OpenCode, which serves ACP as a subcommand.
    pub fn opencode() -> Self {
        Self::new("opencode", "OpenCode", "opencode", &["acp"])
    }

    fn new(id: &'static str, display_name: &'static str, program: &str, args: &[&str]) -> Self {
        Self {
            id,
            display_name,
            program: program.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            env: Vec::new(),
        }
    }

    /// Run `program` instead of the default binary.
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    /// Set an environment variable for every process this driver starts.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// This driver's half of one turn's exchange.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpState {
    /// What `session/prompt` sends, once there is a session to send it to.
    prompt: String,
    cwd: String,
    /// The `mcpServers` array, already in ACP's shape.
    mcp_servers: String,
    /// The conversation to reload, when this turn continues one.
    resume: Option<String>,
    /// Answer permission requests without asking: the session was started in
    /// [`AccessMode::Auto`].
    auto_approve: bool,
    next_id: u64,
    initialize: Option<u64>,
    open: Option<u64>,
    /// Whether [`AcpState::open`] is a `session/load`, which falls back to a
    /// new session when the agent refuses it.
    loading: bool,
    prompt_id: Option<u64>,
    /// Set while a loaded conversation is replayed: those updates are
    /// already in the transcript.
    replaying: bool,
    /// Tool calls by id, so an update finds its kind and title.
    tools: BTreeMap<String, (ActivityKind, String)>,
}

impl AcpState {
    fn request(&mut self, method: &str, params: Value) -> (u64, String) {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        (id, line.to_string())
    }

    fn mcp_servers(&self) -> Value {
        serde_json::from_str(&self.mcp_servers).unwrap_or_else(|_| json!([]))
    }

    /// Open the conversation: reload it when the agent can, start one when
    /// it cannot.
    fn open(&mut self, can_load: bool, state: &mut ParseState) {
        let (id, line) = match self.resume.clone().filter(|_| can_load) {
            Some(session) => {
                self.loading = true;
                self.replaying = true;
                state.vendor_session_id = Some(session.clone());
                self.request(
                    "session/load",
                    json!({"sessionId": session, "cwd": self.cwd, "mcpServers": self.mcp_servers()}),
                )
            }
            None => {
                self.loading = false;
                self.request(
                    "session/new",
                    json!({"cwd": self.cwd, "mcpServers": self.mcp_servers()}),
                )
            }
        };
        self.open = Some(id);
        state.outbox.push(line);
    }

    fn send_prompt(&mut self, session: &str, state: &mut ParseState) {
        let (id, line) = self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": self.prompt}]}),
        );
        self.prompt_id = Some(id);
        state.outbox.push(line);
    }
}

impl AgentDriver for AcpDriver {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    /// The agent chooses; ACP's model selection is not stable yet.
    fn models(&self) -> Vec<ProviderModel> {
        Vec::new()
    }

    fn program(&self) -> &str {
        &self.program
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec::new(&self.program).arg("--version")
    }

    /// The first word that is a number: `0.9.1`, `v1.2.3`, `opencode 0.4.2`.
    fn parse_version(&self, output: &str) -> Option<String> {
        output
            .split_whitespace()
            .map(|word| word.trim_start_matches('v'))
            .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(str::to_string)
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program).args(self.args.iter().cloned());
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    /// The same process: which conversation to continue is said over the
    /// protocol, in [`AgentDriver::begin`].
    fn resume_command(&self, spec: &SessionSpec, _vendor_session_id: &str) -> CommandSpec {
        self.start_command(spec)
    }

    fn begin(
        &self,
        spec: &SessionSpec,
        vendor_session_id: Option<&str>,
        state: &mut ParseState,
    ) -> Vec<String> {
        let servers: Vec<Value> = spec
            .mcp_servers
            .iter()
            .map(|server| {
                json!({
                    "name": server.name,
                    "command": server.command,
                    "args": server.args,
                    "env": server
                        .env
                        .iter()
                        .map(|(name, value)| json!({"name": name, "value": value}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let acp = &mut state.acp;
        acp.prompt = spec.agent_prompt();
        acp.cwd = spec.workspace_path.display().to_string();
        acp.mcp_servers = Value::Array(servers).to_string();
        acp.resume = vendor_session_id.map(str::to_string);
        acp.auto_approve = spec.access_mode == AccessMode::Auto;
        let (id, line) = acp.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                // Files and terminals stay the agent's own: Ginka has no
                // editor buffer to offer, and the worktree is on the same disk.
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false,
                },
            }),
        );
        acp.initialize = Some(id);
        vec![line]
    }

    fn supports_responses(&self) -> bool {
        true
    }

    /// `request_id` is the one [`permission_request`] made: the JSON-RPC id
    /// and the options offered, so the answer can name the option chosen.
    fn encode_response(&self, request_id: &str, response: &str) -> Option<String> {
        let asked: Value = serde_json::from_str(request_id).ok()?;
        let chosen = asked
            .get("options")?
            .as_array()?
            .iter()
            .filter_map(Value::as_array)
            .find(|pair| pair.first().and_then(Value::as_str) == Some(response.trim()))
            .and_then(|pair| pair.get(1))
            .and_then(Value::as_str);
        let outcome = match chosen {
            Some(option) => json!({"outcome": "selected", "optionId": option}),
            // Anything that is not one of the offered choices declines.
            None => json!({"outcome": "cancelled"}),
        };
        Some(
            json!({"jsonrpc": "2.0", "id": asked.get("rpc")?, "result": {"outcome": outcome}})
                .to_string(),
        )
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<AgentEvent> {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            state.unrecognized += 1;
            return Vec::new();
        };
        if value.get("jsonrpc").is_none() {
            state.unrecognized += 1;
            return Vec::new();
        }
        state.recognized += 1;
        let method = value.get("method").and_then(Value::as_str);
        let id = value.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (None, Some(id)) => answer(id.as_u64(), &value, state),
            (Some("session/request_permission"), Some(id)) => {
                permission_request(id, &value["params"], state)
            }
            (Some(_), Some(id)) => {
                // Nothing else was offered in `clientCapabilities`; say so
                // rather than leave the agent waiting.
                state.outbox.push(
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": METHOD_NOT_FOUND, "message": "not supported by this client"},
                    })
                    .to_string(),
                );
                Vec::new()
            }
            (Some("session/update"), None) if !state.acp.replaying => {
                update(&value["params"]["update"], state)
            }
            _ => Vec::new(),
        }
    }
}

/// An answer to one of this driver's requests.
fn answer(id: Option<u64>, value: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let error = value.get("error").map(|error| {
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the agent refused the request")
            .to_string()
    });
    let result = value.get("result").cloned().unwrap_or(Value::Null);
    let asked = |mine: Option<u64>| id.is_some() && id == mine;
    if asked(state.acp.initialize) {
        if let Some(error) = error {
            return failed(error);
        }
        let can_load = result
            .pointer("/agentCapabilities/loadSession")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut acp = std::mem::take(&mut state.acp);
        acp.open(can_load, state);
        state.acp = acp;
        return Vec::new();
    }
    if asked(state.acp.open) {
        state.acp.replaying = false;
        if let Some(error) = error {
            if !state.acp.loading {
                return failed(error);
            }
            // A conversation the agent no longer has: start again with what
            // the prompt says rather than fail the turn.
            let mut acp = std::mem::take(&mut state.acp);
            acp.resume = None;
            state.vendor_session_id = None;
            acp.open(false, state);
            state.acp = acp;
            return Vec::new();
        }
        let session = if state.acp.loading {
            state.vendor_session_id.clone()
        } else {
            result
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let Some(session) = session else {
            return failed("the agent opened no session".to_string());
        };
        state.vendor_session_id = Some(session.clone());
        let mut acp = std::mem::take(&mut state.acp);
        acp.send_prompt(&session, state);
        state.acp = acp;
        return vec![AgentEvent::Connected {
            session_id: Some(session),
            model: None,
        }];
    }
    if asked(state.acp.prompt_id) {
        if let Some(error) = error {
            return failed(error);
        }
        state.turn += 1;
        let outcome = match result.get("stopReason").and_then(Value::as_str) {
            Some("cancelled") => (SessionState::Cancelled, None),
            Some("refusal") => (
                SessionState::Failed,
                Some("the agent refused the prompt".to_string()),
            ),
            Some("max_tokens") => (
                SessionState::Finished,
                Some("stopped at the token limit".to_string()),
            ),
            _ => (SessionState::Finished, None),
        };
        return vec![
            AgentEvent::TurnEnd { turn: state.turn },
            AgentEvent::SessionResult {
                state: outcome.0,
                summary: outcome.1,
            },
        ];
    }
    Vec::new()
}

/// The turn cannot go on. Ending it is what closes the agent's input.
fn failed(message: String) -> Vec<AgentEvent> {
    vec![AgentEvent::SessionResult {
        state: SessionState::Failed,
        summary: Some(message),
    }]
}

/// The agent asks before doing something: answered at once in `auto`, put in
/// front of the reader otherwise.
fn permission_request(id: &Value, params: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let options: Vec<(String, String, String)> = params
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| {
            Some((
                option.get("name")?.as_str()?.to_string(),
                option.get("optionId")?.as_str()?.to_string(),
                option
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect();
    if state.acp.auto_approve
        && let Some((_, option, _)) = options
            .iter()
            .find(|(_, _, kind)| kind == "allow_once")
            .or_else(|| {
                options
                    .iter()
                    .find(|(_, _, kind)| kind.starts_with("allow"))
            })
    {
        state.outbox.push(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"outcome": {"outcome": "selected", "optionId": option}},
            })
            .to_string(),
        );
        return Vec::new();
    }
    let question = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .pointer("/toolCall/toolCallId")
                .and_then(Value::as_str)
                .and_then(|call| state.acp.tools.get(call).map(|(_, title)| title.as_str()))
        })
        .unwrap_or("The agent asks for permission")
        .to_string();
    let request = json!({
        "rpc": id,
        "options": options
            .iter()
            .map(|(name, option, _)| json!([name, option]))
            .collect::<Vec<_>>(),
    });
    vec![AgentEvent::AskUser {
        id: request.to_string(),
        question,
        options: options.into_iter().map(|(name, _, _)| name).collect(),
        questions: Vec::new(),
    }]
}

/// One `session/update`.
fn update(update: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let text = || {
        update
            .pointer("/content/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => {
            state.streaming = true;
            let text = text();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![AgentEvent::TextDelta { text }]
            }
        }
        Some("agent_thought_chunk") => {
            let text = text();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![AgentEvent::Reasoning { text }]
            }
        }
        Some("tool_call") => {
            let Some(call) = update.get("toolCallId").and_then(Value::as_str) else {
                return Vec::new();
            };
            let kind = activity_kind(update.get("kind").and_then(Value::as_str));
            let title = update
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(kind.as_str())
                .to_string();
            state
                .acp
                .tools
                .insert(call.to_string(), (kind, title.clone()));
            let mut events = vec![AgentEvent::ToolCall {
                activity: activity(call, kind, &title, None, false, false),
            }];
            events.extend(settled(call, update, state));
            events
        }
        Some("tool_call_update") => {
            let Some(call) = update.get("toolCallId").and_then(Value::as_str) else {
                return Vec::new();
            };
            if let Some(title) = update.get("title").and_then(Value::as_str)
                && let Some(entry) = state.acp.tools.get_mut(call)
            {
                entry.1 = title.to_string();
            }
            settled(call, update, state)
        }
        Some("plan") => {
            let tasks: Vec<TaskItem> = update
                .get("entries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let status = match entry.get("status").and_then(Value::as_str)? {
                        "completed" => TaskStatus::Completed,
                        "in_progress" => TaskStatus::InProgress,
                        _ => TaskStatus::Pending,
                    };
                    Some(TaskItem::new(entry.get("content")?.as_str()?, status))
                })
                .take(ActivityItem::MAX_TASKS)
                .collect();
            vec![AgentEvent::ToolCall {
                activity: ActivityItem {
                    tasks: Some(tasks),
                    ..activity("plan", ActivityKind::Plan, "Plan", None, false, true)
                },
            }]
        }
        Some("available_commands_update") => vec![AgentEvent::Commands {
            commands: update
                .get("availableCommands")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|command| command.get("name")?.as_str().map(str::to_string))
                .collect(),
        }],
        _ => Vec::new(),
    }
}

/// The result of a call, once its status says it is over.
fn settled(call: &str, update: &Value, state: &ParseState) -> Vec<AgentEvent> {
    let failed = match update.get("status").and_then(Value::as_str) {
        Some("completed") => false,
        Some("failed") => true,
        _ => return Vec::new(),
    };
    let (kind, title) = state
        .acp
        .tools
        .get(call)
        .cloned()
        .unwrap_or((ActivityKind::Tool, "tool".to_string()));
    let detail: Vec<&str> = update
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|content| content.pointer("/content/text").and_then(Value::as_str))
        .collect();
    let detail = (!detail.is_empty()).then(|| detail.join("\n"));
    vec![AgentEvent::ToolResult {
        activity: activity(call, kind, &title, detail, failed, true),
    }]
}

fn activity(
    id: &str,
    kind: ActivityKind,
    title: &str,
    detail: Option<String>,
    failed: bool,
    complete: bool,
) -> ActivityItem {
    ActivityItem {
        id: Some(id.to_string()),
        kind,
        title: title.to_string(),
        tasks: None,
        detail,
        failed,
        complete,
    }
}

/// ACP's tool kinds, in our vocabulary.
fn activity_kind(kind: Option<&str>) -> ActivityKind {
    match kind {
        Some("execute") => ActivityKind::Command,
        Some("edit" | "delete" | "move") => ActivityKind::FileChange,
        Some("search") => ActivityKind::Search,
        Some("think") => ActivityKind::Reasoning,
        _ => ActivityKind::Tool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begun(access: AccessMode, resume: Option<&str>) -> (AcpDriver, ParseState, Vec<String>) {
        let driver = AcpDriver::gemini();
        let mut state = ParseState::default();
        let spec = SessionSpec::new("/tmp/wt", "hello").with_access_mode(access);
        let opening = driver.begin(&spec, resume, &mut state);
        (driver, state, opening)
    }

    fn feed(driver: &AcpDriver, state: &mut ParseState, line: Value) -> Vec<AgentEvent> {
        driver.parse_line(&line.to_string(), state)
    }

    fn sent(state: &mut ParseState) -> Vec<Value> {
        state
            .outbox
            .drain(..)
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect()
    }

    #[test]
    fn the_cli_is_started_in_acp_mode() {
        let command = AcpDriver::opencode().start_command(&SessionSpec::new("/tmp/wt", "x"));
        assert_eq!(command.program, "opencode");
        assert_eq!(command.args, vec!["acp"]);
        assert_eq!(
            AcpDriver::gemini()
                .start_command(&SessionSpec::new("/tmp/wt", "x"))
                .args,
            vec!["--experimental-acp"]
        );
    }

    #[test]
    fn a_turn_is_initialize_then_a_new_session_then_the_prompt() {
        let (driver, mut state, opening) = begun(AccessMode::Ask, None);
        let first: Value = serde_json::from_str(&opening[0]).unwrap();
        assert_eq!(first["method"], "initialize");
        assert_eq!(first["params"]["protocolVersion"], 1);

        feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}}),
        );
        let open = sent(&mut state);
        assert_eq!(open[0]["method"], "session/new");
        assert_eq!(open[0]["params"]["cwd"], "/tmp/wt");

        let events = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s-1"}}),
        );
        assert_eq!(state.vendor_session_id.as_deref(), Some("s-1"));
        assert!(matches!(&events[..], [AgentEvent::Connected { .. }]));
        let prompt = sent(&mut state);
        assert_eq!(prompt[0]["method"], "session/prompt");
        assert_eq!(prompt[0]["params"]["sessionId"], "s-1");
        assert_eq!(prompt[0]["params"]["prompt"][0]["text"], "hello");

        let said = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s-1",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}}}),
        );
        assert_eq!(said, vec![AgentEvent::TextDelta { text: "hi".into() }]);

        let end = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 3, "result": {"stopReason": "end_turn"}}),
        );
        assert_eq!(
            end,
            vec![
                AgentEvent::TurnEnd { turn: 1 },
                AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    summary: None
                }
            ]
        );
    }

    #[test]
    fn a_conversation_is_reloaded_where_the_agent_can_and_its_replay_is_not_recorded() {
        let (driver, mut state, _) = begun(AccessMode::Ask, Some("s-9"));
        feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"agentCapabilities": {"loadSession": true}}}),
        );
        let open = sent(&mut state);
        assert_eq!(open[0]["method"], "session/load");
        assert_eq!(open[0]["params"]["sessionId"], "s-9");
        let replay = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s-9",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "old"}}}}),
        );
        assert!(replay.is_empty(), "already in the transcript");
        feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 2, "result": null}),
        );
        assert_eq!(sent(&mut state)[0]["params"]["sessionId"], "s-9");
    }

    #[test]
    fn a_conversation_the_agent_cannot_reload_starts_a_new_one() {
        let (driver, mut state, _) = begun(AccessMode::Ask, Some("s-9"));
        feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
        );
        assert_eq!(sent(&mut state)[0]["method"], "session/new");
    }

    #[test]
    fn a_permission_request_is_asked_and_answered_with_the_option_chosen() {
        let (driver, mut state, _) = begun(AccessMode::Ask, None);
        let events = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": {
            "sessionId": "s-1",
            "toolCall": {"toolCallId": "c1", "title": "Run cargo test"},
            "options": [
                {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                {"optionId": "no", "name": "Reject", "kind": "reject_once"}
            ]}}),
        );
        let [
            AgentEvent::AskUser {
                id,
                question,
                options,
                ..
            },
        ] = &events[..]
        else {
            panic!("expected a question, got {events:?}");
        };
        assert_eq!(question, "Run cargo test");
        assert_eq!(options, &["Allow", "Reject"]);
        let reply: Value =
            serde_json::from_str(&driver.encode_response(id, "Allow").unwrap()).unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["outcome"]["optionId"], "yes");
        let declined: Value =
            serde_json::from_str(&driver.encode_response(id, "something else").unwrap()).unwrap();
        assert_eq!(declined["result"]["outcome"]["outcome"], "cancelled");
    }

    #[test]
    fn in_auto_a_permission_request_is_allowed_without_asking() {
        let (driver, mut state, _) = begun(AccessMode::Auto, None);
        let events = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": {
            "options": [
                {"optionId": "always", "name": "Always", "kind": "allow_always"},
                {"optionId": "once", "name": "Allow", "kind": "allow_once"}
            ]}}),
        );
        assert!(events.is_empty());
        assert_eq!(
            sent(&mut state)[0]["result"]["outcome"]["optionId"],
            "once",
            "the narrowest allowance"
        );
    }

    #[test]
    fn a_tool_call_arrives_as_a_call_and_its_result() {
        let (driver, mut state, _) = begun(AccessMode::Ask, None);
        let call = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {
                "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "cargo test",
                "kind": "execute", "status": "pending"}}}),
        );
        assert!(matches!(
            &call[..],
            [AgentEvent::ToolCall { activity }] if activity.kind == ActivityKind::Command
        ));
        let result = feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {"update": {
                "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "failed",
                "content": [{"type": "content", "content": {"type": "text", "text": "1 failed"}}]}}}),
        );
        let [AgentEvent::ToolResult { activity }] = &result[..] else {
            panic!("expected a result, got {result:?}");
        };
        assert!(activity.failed);
        assert_eq!(activity.title, "cargo test");
        assert_eq!(activity.detail.as_deref(), Some("1 failed"));
    }

    #[test]
    fn a_request_this_client_does_not_serve_is_refused_rather_than_left_waiting() {
        let (driver, mut state, _) = begun(AccessMode::Ask, None);
        feed(
            &driver,
            &mut state,
            json!({"jsonrpc": "2.0", "id": 4, "method": "fs/read_text_file", "params": {}}),
        );
        let reply = sent(&mut state);
        assert_eq!(reply[0]["id"], 4);
        assert_eq!(reply[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_line_that_is_not_json_rpc_is_counted() {
        let (driver, mut state, _) = begun(AccessMode::Ask, None);
        driver.parse_line("Loading extensions…", &mut state);
        assert_eq!(state.unrecognized, 1);
        assert!(state.understood_nothing());
    }

    #[test]
    fn the_version_is_the_first_number_printed() {
        let driver = AcpDriver::opencode();
        assert_eq!(driver.parse_version("0.9.1\n").as_deref(), Some("0.9.1"));
        assert_eq!(
            driver.parse_version("opencode v1.2.3").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(driver.parse_version("unknown"), None);
    }
}
