//! The Codex driver.
//!
//! Turns run over `codex app-server` by default (`codex_app`), where Codex
//! can ask before it acts. With `agents.codex.transport = "exec"` they run
//! `codex exec --json` instead, which is what the rest of this module reads:
//! one JSON object per line. Two
//! generations of that format are in the wild — a `thread`/`item`/`turn`
//! vocabulary, and an older envelope with the payload under `msg` — and both
//! are handled, because crate::driver::ActivityItem;
//!
//! As with every driver, the shapes are pinned by the fixtures below rather
//! than by a live session.

use super::{
    ActivityItem, AgentDriver, CommandSpec, CompactionSpec, ModelCatalogueProbe, ParseState,
    PlanUsageProbe, ProviderModel, SessionSpec,
};
use ginka_protocol::model::{PlanUsage, PlanWindow, SessionState};
use ginka_protocol::provider::{OptionOutcome, ProviderOption, SessionOptions};
use ginka_protocol::{AgentEvent, ContextUsage, Usage};
use serde_json::Value;

/// The Codex CLI.
#[derive(Debug, Clone)]
pub struct CodexDriver {
    program: String,
    env: Vec<(String, String)>,
    /// Run turns with `codex exec` rather than over the app server
    /// (`codex_app`): no approvals, but no experimental protocol either.
    exec: bool,
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
            exec: false,
        }
    }

    /// Run turns with `codex exec`, which never asks before it acts.
    pub fn with_exec(mut self) -> Self {
        self.exec = true;
        self
    }

    /// Set an environment variable for every process this driver starts.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// `-c key=value` per MCP server field: how a server reaches one
    /// session without touching the user's `config.toml`.
    fn mcp_args(spec: &SessionSpec) -> Vec<String> {
        crate::tools::codex_overrides(&spec.mcp_servers)
            .into_iter()
            .flat_map(|assignment| ["-c".to_string(), assignment])
            .collect()
    }

    fn model_args(spec: &SessionSpec) -> Vec<String> {
        let mut args = match &spec.model {
            Some(model) => vec!["--model".to_string(), model.clone()],
            None => Vec::new(),
        };
        if let Some(effort) = &spec.reasoning_effort {
            args.extend([
                "-c".to_string(),
                format!("model_reasoning_effort=\"{effort}\""),
            ]);
        }
        if let Some(tier) = &spec.service_tier {
            args.extend(["-c".to_string(), format!("service_tier=\"{tier}\"")]);
        }
        // Our access modes in the vendor's vocabulary, only when they are
        // not what `codex exec` does on its own.
        match spec.access_mode {
            ginka_protocol::provider::AccessMode::Ask => {}
            ginka_protocol::provider::AccessMode::ReadOnly => {
                args.push("--sandbox".to_string());
                args.push("read-only".to_string());
            }
            ginka_protocol::provider::AccessMode::Auto => args.push("--full-auto".to_string()),
        }
        args
    }
}

impl AgentDriver for CodexDriver {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn compaction(&self, _spec: &SessionSpec, vendor_session_id: &str) -> Option<CompactionSpec> {
        Some(CompactionSpec {
            command: CommandSpec::new(&self.program).arg("app-server"),
            input: vec![
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"clientInfo": {"name": "ginka", "title": "Ginka", "version": env!("CARGO_PKG_VERSION")}}
                })
                .to_string(),
                serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})
                    .to_string(),
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 2, "method": "thread/compact/start",
                    "params": {"threadId": vendor_session_id}
                })
                .to_string(),
            ],
        })
    }

    fn models(&self) -> Vec<ProviderModel> {
        // Stable family ids as the offline fallback. The app-server catalogue
        // wins whenever it answers, so new and retired models do not wait for
        // a Ginka release (`docs/roadmap.md` §3.3 N3).
        ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
            .into_iter()
            .map(|id| {
                ProviderModel::new(id, id).with_reasoning_efforts(
                    ["low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .map(|effort| ProviderOption::new(effort, effort)),
                )
            })
            .collect()
    }

    fn apply_options(&self, _before: &SessionOptions, _after: &SessionOptions) -> OptionOutcome {
        // `codex exec resume` accepts the same model/config overrides as a
        // fresh turn, so the provider thread remains valid.
        OptionOutcome::Absorbed
    }

    fn model_catalogue_probe(&self) -> Option<ModelCatalogueProbe> {
        Some(ModelCatalogueProbe {
            command: CommandSpec::new(&self.program)
                .arg("app-server")
                .arg("--stdio"),
            input: vec![
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"clientInfo": {"name": "ginka", "title": "Ginka", "version": env!("CARGO_PKG_VERSION")}}
                })
                .to_string(),
                serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})
                    .to_string(),
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 2, "method": "model/list",
                    "params": {"limit": 100, "includeHidden": false}
                })
                .to_string(),
            ],
        })
    }

    fn parse_model_catalogue(&self, line: &str) -> Option<Vec<ProviderModel>> {
        let value: Value = serde_json::from_str(line.trim()).ok()?;
        let rows = value.get("result")?.get("data")?.as_array()?;
        Some(
            rows.iter()
                .filter(|row| !row.get("hidden").and_then(Value::as_bool).unwrap_or(false))
                .filter_map(|row| {
                    let id = row.get("id")?.as_str()?;
                    let label = row.get("displayName").and_then(Value::as_str).unwrap_or(id);
                    let efforts = row
                        .get("supportedReasoningEfforts")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|option| {
                            let id = option.get("reasoningEffort")?.as_str()?;
                            Some(ProviderOption::new(id, id))
                        });
                    let tiers = row
                        .get("serviceTiers")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|option| {
                            let id = option.get("id")?.as_str()?;
                            let label = option.get("name").and_then(Value::as_str).unwrap_or(id);
                            Some(ProviderOption::new(id, label))
                        });
                    let mut model = ProviderModel::new(id, label)
                        .with_reasoning_efforts(efforts)
                        .with_service_tiers(tiers);
                    model.is_default = row
                        .get("isDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Some(model)
                })
                .collect(),
        )
    }

    fn program(&self) -> &str {
        &self.program
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec::new(&self.program).arg("--version")
    }

    /// `codex-cli 0.144.1` — the number is the last word.
    fn parse_version(&self, output: &str) -> Option<String> {
        let version = output.split_whitespace().next_back()?;
        version
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
            .then(|| version.to_string())
    }

    fn auth_command(&self) -> Option<CommandSpec> {
        Some(CommandSpec::new(&self.program).arg("login").arg("status"))
    }

    /// A sentence rather than a document: `Logged in using ChatGPT`.
    fn parse_auth(&self, output: &str) -> Option<(bool, Option<String>)> {
        let said = output.trim();
        let lowered = said.to_ascii_lowercase();
        if lowered.contains("not logged in") || lowered.contains("not authenticated") {
            return Some((false, None));
        }
        if lowered.contains("logged in") {
            return Some((true, Some(said.lines().next()?.to_string())));
        }
        None
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        if !self.exec {
            // One process for the turn; the prompt and the thread are said
            // over the protocol (`begin`). MCP servers ride `-c` overrides,
            // which the server reads as `exec` does.
            let mut command = CommandSpec::new(&self.program)
                .arg("app-server")
                .args(Self::mcp_args(spec));
            for (key, value) in self.env.iter().chain(spec.env.iter()) {
                command = command.env(key, value);
            }
            return command;
        }
        let mut command = CommandSpec::new(&self.program)
            .arg("exec")
            .arg("--json")
            .args(Self::model_args(spec))
            .args(Self::mcp_args(spec));
        command.args.push(spec.agent_prompt());
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec {
        if !self.exec {
            // Which thread to continue is said in `begin`.
            return self.start_command(spec);
        }
        let mut command = CommandSpec::new(&self.program)
            .arg("exec")
            .arg("resume")
            .arg(vendor_session_id)
            .arg("--json")
            .args(Self::model_args(spec))
            .args(Self::mcp_args(spec));
        command.args.push(spec.agent_prompt());
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    /// Codex reads everything — login, config, sessions — from `~/.codex`
    /// unless this says otherwise.
    fn home_variable(&self) -> Option<&'static str> {
        Some("CODEX_HOME")
    }

    fn login_command(&self) -> Option<CommandSpec> {
        Some(CommandSpec::new(&self.program).arg("login"))
    }

    /// The app server answers `account/rateLimits/read` over stdio once it
    /// has been initialized. Verified against 0.142.5, which is also where
    /// the modern `exec --json` stream turned out to carry no rate limits:
    /// this is the way to a reading, not a fallback.
    fn plan_usage_probe(&self) -> Option<PlanUsageProbe> {
        Some(PlanUsageProbe {
            command: CommandSpec::new(&self.program).arg("app-server"),
            input: vec![
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"clientInfo": {"name": "ginka", "title": "Ginka", "version": env!("CARGO_PKG_VERSION")}}
                })
                .to_string(),
                serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}).to_string(),
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}
                })
                .to_string(),
            ],
        })
    }

    fn parse_plan_usage(&self, line: &str) -> Option<PlanUsage> {
        let value: Value = serde_json::from_str(line.trim()).ok()?;
        plan_usage_from(value.get("result")?.get("rateLimits")?)
    }

    fn begin(
        &self,
        spec: &SessionSpec,
        vendor_session_id: Option<&str>,
        state: &mut ParseState,
    ) -> Vec<String> {
        if self.exec {
            return Vec::new();
        }
        crate::driver::codex_app::begin(spec, vendor_session_id, state)
    }

    fn supports_responses(&self) -> bool {
        !self.exec
    }

    /// `request_id` is the card `codex_app` made for the server's request.
    fn encode_response(&self, request_id: &str, response: &str) -> Option<String> {
        crate::driver::codex_app::response(request_id, response)
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(mut value) = serde_json::from_str::<Value>(line) else {
            state.unrecognized += 1;
            return Vec::new();
        };
        if state.codex.active {
            return crate::driver::codex_app::parse(&value, state);
        }

        // The app-server spells the same lifecycle as JSON-RPC notifications.
        // Normalize its standard turn/item events; request responses are
        // acknowledgements rather than transcript content.
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            if method == "turn/failed" {
                state.recognized += 1;
                state.turn += 1;
                let summary = value
                    .pointer("/params/turn/error/message")
                    .or_else(|| value.pointer("/params/error/message"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return vec![
                    AgentEvent::TurnEnd { turn: state.turn },
                    AgentEvent::SessionResult {
                        state: SessionState::Failed,
                        summary,
                    },
                ];
            }
            let event_type = match method {
                "turn/started" => Some("turn.started"),
                "turn/completed" => Some("turn.completed"),
                "item/started" => Some("item.started"),
                "item/updated" => Some("item.updated"),
                "item/completed" => Some("item.completed"),
                _ => None,
            };
            if let Some(event_type) = event_type {
                let mut normalized = value.get("params").cloned().unwrap_or_default();
                normalized["type"] = Value::String(event_type.to_string());
                value = normalized;
            } else {
                state.recognized += 1;
                return Vec::new();
            }
        } else if value.get("id").is_some()
            && value.get("msg").is_none()
            && value.get("type").is_none()
        {
            state.recognized += 1;
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                state.turn += 1;
                return vec![
                    AgentEvent::TurnEnd { turn: state.turn },
                    AgentEvent::SessionResult {
                        state: SessionState::Failed,
                        summary: Some(message.to_string()),
                    },
                ];
            }
            return Vec::new();
        }

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
            Some("thread.started" | "session.created" | "turn.started") => {
                state.recognized += 1;
                Vec::new()
            }
            Some("item.started" | "item.updated") => {
                state.recognized += 1;
                value
                    .get("item")
                    .map(|item| parse_live_item(item, false))
                    .unwrap_or_default()
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
                if let Some(usage) = context_usage_from(&value) {
                    events.push(AgentEvent::ContextUsage { usage });
                }
                if let Some(usage) = value.get("rate_limits").and_then(plan_usage_from) {
                    events.push(AgentEvent::PlanUsage { usage });
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
            let activity = ActivityItem::from_tool(
                Some(id),
                "shell",
                &serde_json::json!({ "command": command }),
            );
            let mut finished = activity.clone();
            finished.complete_with(
                item.get("aggregated_output")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                item.get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0),
            );
            vec![
                AgentEvent::ToolCall { activity },
                AgentEvent::ToolResult { activity: finished },
            ]
        }
        "file_change" => vec![AgentEvent::ToolCall {
            activity: ActivityItem::from_tool(
                Some(id),
                "edit",
                &item.get("changes").cloned().unwrap_or(Value::Null),
            ),
        }],
        "todo_list" => parse_live_item(item, true),
        _ => Vec::new(),
    }
}

/// Normalize the one modern item whose updates matter before completion.
fn parse_live_item(item: &Value, complete: bool) -> Vec<AgentEvent> {
    let kind = item
        .get("item_type")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind != "todo_list" {
        return Vec::new();
    }
    let tasks = item
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| {
            let text = task.get("text").and_then(Value::as_str)?;
            Some(serde_json::json!({
                "text": text,
                "status": if task.get("completed").and_then(Value::as_bool) == Some(true) {
                    "completed"
                } else {
                    "pending"
                }
            }))
        })
        .collect::<Vec<_>>();
    let mut activity = ActivityItem::from_tool(
        item.get("id").and_then(Value::as_str).map(str::to_string),
        "update_plan",
        &serde_json::json!({ "tasks": tasks }),
    );
    if activity.tasks.is_none() {
        return Vec::new();
    }
    if complete {
        activity.complete_with("", false);
        vec![AgentEvent::ToolResult { activity }]
    } else {
        vec![AgentEvent::ToolCall { activity }]
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
                activity: ActivityItem::from_tool(
                    msg.get("call_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    "shell",
                    &msg.get("command").cloned().unwrap_or(Value::Null),
                ),
            }]
        }
        Some("exec_command_end") => {
            state.recognized += 1;
            let mut activity = ActivityItem::from_tool(
                msg.get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                "shell",
                &Value::Null,
            );
            activity.complete_with(
                msg.get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                msg.get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0),
            );
            vec![AgentEvent::ToolResult { activity }]
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
            let info = msg.get("info").unwrap_or(msg);
            let mut events = vec![AgentEvent::Usage {
                usage: usage_from(info),
            }];
            if let Some(usage) = context_usage_from(info) {
                events.push(AgentEvent::ContextUsage { usage });
            }
            // The older stream carries the account's windows beside the
            // counts; the reading is free, so it is taken.
            if let Some(usage) = msg.get("rate_limits").and_then(plan_usage_from) {
                events.push(AgentEvent::PlanUsage { usage });
            }
            events
        }
        _ => {
            state.unrecognized += 1;
            Vec::new()
        }
    }
}

/// The account's rate-limit windows as Codex describes them: a `primary` and
/// a `secondary` window, each with a used percentage, a length in minutes and
/// a reset time — snake_case on the event stream, camelCase from the app
/// server. `None` when there is no window in it, which is not a reading.
pub(super) fn plan_usage_from(limits: &Value) -> Option<PlanUsage> {
    let number = |value: &Value, keys: &[&str]| -> Option<f64> {
        keys.iter()
            .find_map(|key| value.get(*key))
            .and_then(Value::as_f64)
    };
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(window) = limits.get(key).filter(|window| !window.is_null()) else {
            continue;
        };
        let Some(used_percent) = number(window, &["used_percent", "usedPercent"]) else {
            continue;
        };
        let minutes = number(window, &["window_minutes", "windowDurationMins"]).map(|m| m as i64);
        windows.push(PlanWindow {
            label: window_label(minutes, key),
            used_percent,
            resets_at: number(window, &["resets_at", "resetsAt"]).map(|at| at as i64),
        });
    }
    if windows.is_empty() {
        return None;
    }
    Some(PlanUsage {
        plan: limits
            .get("plan_type")
            .or_else(|| limits.get("planType"))
            .and_then(Value::as_str)
            .map(str::to_string),
        windows,
    })
}

/// What a window is called, from its length: `5h`, `week`. The vendor's own
/// names for them — `primary`, `secondary` — say nothing to a reader, and are
/// only used when the length is missing.
fn window_label(minutes: Option<i64>, fallback: &str) -> String {
    const DAY: i64 = 24 * 60;
    match minutes {
        Some(minutes) if minutes == 7 * DAY => "week".to_string(),
        Some(minutes) if minutes > 0 && minutes % DAY == 0 => format!("{}d", minutes / DAY),
        Some(minutes) if minutes > 0 && minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) if minutes > 0 => format!("{minutes}m"),
        _ => fallback.to_string(),
    }
}

/// Codex's accounting, under whichever names this generation uses.
fn usage_from(usage: &Value) -> Usage {
    let usage = usage
        .get("total_token_usage")
        .or_else(|| usage.get("totalTokenUsage"))
        .unwrap_or(usage);
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

/// Read current context occupancy only when the vendor supplied a capacity and
/// a last-request total together. Session totals are not context occupancy:
/// they continue increasing after the provider compacts a thread.
pub(super) fn context_usage_from(value: &Value) -> Option<ContextUsage> {
    let usage = value
        .get("tokenUsage")
        .or_else(|| value.get("usage"))
        .unwrap_or(value);
    let window_tokens = ["model_context_window", "modelContextWindow"]
        .into_iter()
        .find_map(|key| usage.get(key).and_then(Value::as_u64))?;
    if window_tokens == 0 {
        return None;
    }
    let last = ["last_token_usage", "lastTokenUsage", "last"]
        .into_iter()
        .find_map(|key| usage.get(key))?;
    let used_tokens = ["total_tokens", "totalTokens"]
        .into_iter()
        .find_map(|key| last.get(key).and_then(Value::as_u64))
        .unwrap_or_else(|| {
            let input = ["input_tokens", "inputTokens"]
                .into_iter()
                .find_map(|key| last.get(key).and_then(Value::as_u64))
                .unwrap_or(0);
            let output = ["output_tokens", "outputTokens"]
                .into_iter()
                .find_map(|key| last.get(key).and_then(Value::as_u64))
                .unwrap_or(0);
            input.saturating_add(output)
        });
    Some(ContextUsage {
        used_tokens,
        window_tokens,
        can_compact: true,
    })
}

#[cfg(test)]
mod tests {

    use super::*;
    use ginka_protocol::AccessMode;

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
    fn a_turn_runs_over_the_app_server_unless_exec_is_asked_for() {
        let driver = CodexDriver::default();
        let spec =
            SessionSpec::new("/w", "hello").with_mcp_servers(vec![crate::tools::McpServer {
                name: "ginka".into(),
                command: "ginka".into(),
                args: vec!["mcp".into()],
                env: Vec::new(),
            }]);
        let command = driver.start_command(&spec);
        assert_eq!(command.args[0], "app-server");
        assert!(
            !command.args.iter().any(|arg| arg == "hello"),
            "the prompt goes over the protocol, not the command line"
        );
        assert!(
            command
                .args
                .iter()
                .any(|arg| arg.starts_with("mcp_servers.ginka"))
        );
        assert_eq!(driver.resume_command(&spec, "th-1").args, command.args);
        assert!(driver.supports_responses());
        let mut state = ParseState::default();
        assert_eq!(driver.begin(&spec, Some("th-1"), &mut state).len(), 3);

        let exec = CodexDriver::default().with_exec();
        assert_eq!(exec.start_command(&spec).args[0], "exec");
        assert!(!exec.supports_responses());
        assert!(
            exec.begin(&spec, None, &mut ParseState::default())
                .is_empty()
        );
    }

    #[test]
    fn the_start_command_asks_for_jsonl() {
        let driver = CodexDriver::default().with_exec();
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
    fn manual_compaction_uses_the_provider_command() {
        let driver = CodexDriver::default();
        let compact = driver
            .compaction(&SessionSpec::new("/tmp/wt", ""), "thread-1")
            .expect("codex exposes app-server compaction");
        assert_eq!(compact.command.args, vec!["app-server"]);
        assert!(compact.input[2].contains("thread/compact/start"));
        assert!(compact.input[2].contains("thread-1"));
    }

    #[test]
    fn app_server_compaction_lifecycle_finishes_the_control_turn() {
        let (events, state) = parse(&[
            r#"{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1"}}}"#,
            r#"{"method":"item/completed","params":{"threadId":"thread-1","item":{"id":"compact-1","type":"contextCompaction"}}}"#,
            r#"{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}}"#,
        ]);

        assert_eq!(state.turn, 1);
        assert!(events.contains(&AgentEvent::TurnEnd { turn: 1 }));
        assert!(events.contains(&AgentEvent::SessionResult {
            state: SessionState::Finished,
            summary: None,
        }));
    }

    #[test]
    fn app_server_compaction_error_closes_the_control_turn() {
        let (events, state) =
            parse(&[r#"{"id":2,"error":{"code":-32602,"message":"thread is busy"}}"#]);

        assert_eq!(state.turn, 1);
        assert_eq!(events[0], AgentEvent::TurnEnd { turn: 1 });
        assert_eq!(
            events[1],
            AgentEvent::SessionResult {
                state: SessionState::Failed,
                summary: Some("thread is busy".into()),
            }
        );
    }

    #[test]
    fn app_server_compaction_failure_notification_closes_the_control_turn() {
        let (events, state) = parse(&[
            r#"{"method":"turn/failed","params":{"turn":{"error":{"message":"compact failed"}}}}"#,
        ]);

        assert_eq!(state.turn, 1);
        assert_eq!(events[0], AgentEvent::TurnEnd { turn: 1 });
        assert_eq!(
            events[1],
            AgentEvent::SessionResult {
                state: SessionState::Failed,
                summary: Some("compact failed".into()),
            }
        );
    }

    #[test]
    fn each_access_mode_is_a_sandbox_the_vendor_names() {
        let driver = CodexDriver::default().with_exec();
        let args_for = |access| {
            driver
                .start_command(&SessionSpec::new("/tmp/wt", "go").with_access_mode(access))
                .args
        };
        let read_only = args_for(AccessMode::ReadOnly);
        assert!(
            read_only
                .windows(2)
                .any(|pair| pair == ["--sandbox", "read-only"])
        );
        // `ask` is what `codex exec` does on its own, so nothing is said.
        let ask = args_for(AccessMode::Ask);
        assert!(
            !ask.iter()
                .any(|arg| arg == "--sandbox" || arg == "--full-auto")
        );
        let auto = args_for(AccessMode::Auto);
        assert!(auto.contains(&"--full-auto".to_string()));
        // Never the flag that drops the sandbox: that stays the user's call.
        assert!(!auto.iter().any(|arg| arg.contains("dangerously")));
        // And a resume runs under the same mode.
        let resumed = driver.resume_command(
            &SessionSpec::new("/tmp/wt", "go").with_access_mode(AccessMode::ReadOnly),
            "01H",
        );
        assert!(
            resumed
                .args
                .windows(2)
                .any(|pair| pair == ["--sandbox", "read-only"])
        );
    }

    #[test]
    fn mcp_servers_become_config_overrides_before_the_prompt() {
        let driver = CodexDriver::default().with_exec();
        let spec =
            SessionSpec::new("/tmp/wt", "hello").with_mcp_servers(vec![crate::tools::McpServer {
                name: "ginka".into(),
                command: "/opt/ginka".into(),
                args: vec!["mcp".into()],
                env: vec![("GINKA_HOME".into(), "/h".into())],
            }]);
        let command = driver.start_command(&spec);
        let overrides: Vec<&str> = command
            .args
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(
            overrides,
            [
                r#"mcp_servers.ginka.command="/opt/ginka""#,
                r#"mcp_servers.ginka.args=["mcp"]"#,
                r#"mcp_servers.ginka.env={GINKA_HOME="/h"}"#,
            ]
        );
        assert_eq!(command.args.last().map(String::as_str), Some("hello"));
        let resumed = driver.resume_command(&spec, "01H");
        assert!(
            resumed
                .args
                .iter()
                .any(|arg| arg.starts_with("mcp_servers.ginka"))
        );
    }

    #[test]
    fn a_preamble_is_sent_in_front_of_the_prompt_and_nowhere_else() {
        let driver = CodexDriver::default().with_exec();
        let spec = SessionSpec::new("/tmp/wt", "carry on")
            .with_preamble(Some("what came before".to_string()));
        let command = driver.start_command(&spec);
        let sent = command.args.last().unwrap();
        assert!(sent.starts_with("what came before"), "{sent}");
        assert!(sent.ends_with("carry on"), "{sent}");
        // Nothing is recorded under the preamble's name: the transcript
        // keeps `prompt`, which is untouched.
        assert_eq!(spec.prompt, "carry on");
        assert_eq!(
            SessionSpec::new("/tmp/wt", "x")
                .with_preamble(Some("  ".to_string()))
                .agent_prompt(),
            "x",
            "a blank preamble is no preamble"
        );
    }

    #[test]
    fn a_resume_names_the_session_before_the_prompt() {
        let driver = CodexDriver::default().with_exec();
        let command = driver.resume_command(&SessionSpec::new("/tmp/wt", "carry on"), "01H");
        assert_eq!(&command.args[..3], &["exec", "resume", "01H"]);
        assert_eq!(command.args.last().map(String::as_str), Some("carry on"));
    }

    #[test]
    fn the_version_is_read_out_of_what_the_cli_prints() {
        let driver = CodexDriver::default();
        assert_eq!(
            driver.parse_version("codex-cli 0.144.1\n").as_deref(),
            Some("0.144.1")
        );
        // A CLI that answered with something else has not given us a version.
        assert_eq!(driver.parse_version("command not found").as_deref(), None);
    }

    #[test]
    fn signing_in_is_read_from_the_sentence_the_cli_prints() {
        let driver = CodexDriver::default();
        assert_eq!(
            driver.parse_auth("Logged in using ChatGPT\n"),
            Some((true, Some("Logged in using ChatGPT".into())))
        );
        assert_eq!(driver.parse_auth("Not logged in\n"), Some((false, None)));
        assert_eq!(driver.parse_auth("something else entirely"), None);
    }

    #[test]
    fn the_cli_catalogue_keeps_model_options_and_hides_hidden_models() {
        let driver = CodexDriver::default();
        let models = driver
            .parse_model_catalogue(
                r#"{"id":2,"result":{"data":[
                    {"id":"gpt-next","displayName":"GPT Next","hidden":false,"isDefault":true,
                     "supportedReasoningEfforts":[
                       {"reasoningEffort":"low","description":"Fast"},
                       {"reasoningEffort":"high","description":"Deep"}],
                     "serviceTiers":[{"id":"priority","name":"Fast","description":"Faster"}]},
                    {"id":"retired","displayName":"Retired","hidden":true,"isDefault":false,
                     "supportedReasoningEfforts":[],"serviceTiers":[]}
                ]}}"#,
            )
            .expect("model/list response");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-next");
        assert_eq!(models[0].label, "GPT Next");
        assert!(models[0].is_default);
        assert!(models[0].supports_reasoning_effort("high"));
        assert!(models[0].supports_service_tier("priority"));
    }

    #[test]
    fn codex_asks_its_app_server_for_the_catalogue() {
        let probe = CodexDriver::default()
            .model_catalogue_probe()
            .expect("Codex exposes model/list");
        assert_eq!(probe.command.args, ["app-server", "--stdio"]);
        assert!(probe.input.iter().any(|line| line.contains("model/list")));
    }

    #[test]
    fn the_static_catalogue_keeps_the_picker_usable_offline() {
        let driver = CodexDriver::default();
        assert!(!driver.models().is_empty());
        assert_eq!(driver.parse_model_catalogue("not json"), None);
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
    fn live_todo_items_become_provider_neutral_task_updates() {
        let (events, state) = parse(&[
            r#"{"type":"item.started","item":{"id":"todo_1","type":"todo_list","items":[{"text":"Inspect","completed":true},{"text":"Implement","completed":false}]}}"#,
            r#"{"type":"item.updated","item":{"id":"todo_1","type":"todo_list","items":[{"text":"Inspect","completed":true},{"text":"Implement","completed":true},{"text":"Verify","completed":false}]}}"#,
            r#"{"type":"item.completed","item":{"id":"todo_1","type":"todo_list","items":[{"text":"Inspect","completed":true},{"text":"Implement","completed":true},{"text":"Verify","completed":true}]}}"#,
        ]);

        assert_eq!(state.unrecognized, 0);
        assert_eq!(events.len(), 3);
        let AgentEvent::ToolCall { activity } = &events[1] else {
            panic!("a live task snapshot is a normalized plan call")
        };
        assert_eq!(activity.kind, ginka_protocol::event::ActivityKind::Plan);
        assert_eq!(
            activity.tasks.as_deref(),
            Some(
                [
                    ginka_protocol::TaskItem::new(
                        "Inspect",
                        ginka_protocol::TaskStatus::Completed,
                    ),
                    ginka_protocol::TaskItem::new(
                        "Implement",
                        ginka_protocol::TaskStatus::Completed,
                    ),
                    ginka_protocol::TaskItem::new(
                        "Verify",
                        ginka_protocol::TaskStatus::Pending,
                    ),
                ]
                .as_slice()
            )
        );
        assert!(matches!(
            events.last(),
            Some(AgentEvent::ToolResult { activity })
                if activity.tasks.as_ref().is_some_and(|tasks| {
                    tasks.iter().all(|task| {
                        task.status == ginka_protocol::TaskStatus::Completed
                    })
                })
        ));
    }

    #[test]
    fn a_command_execution_arrives_as_a_call_and_its_result() {
        let (events, _) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_1","item_type":"command_execution","command":"cargo test","aggregated_output":"ok","exit_code":0}}"#,
        ]);
        // The result completes the call rather than restating it: the row
        // keeps the title it was given.
        let call = ActivityItem::from_tool(
            Some("item_1".into()),
            "shell",
            &serde_json::json!({ "command": "cargo test" }),
        );
        let mut finished = call.clone();
        finished.complete_with("ok", false);
        assert_eq!(
            events,
            vec![
                AgentEvent::ToolCall { activity: call },
                AgentEvent::ToolResult { activity: finished },
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
            [_, AgentEvent::ToolResult { activity }] if activity.failed
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
    fn the_older_stream_reports_the_accounts_windows_beside_the_counts() {
        let (events, _) = parse(&[
            r#"{"id":"1","msg":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5}},"rate_limits":{"primary":{"used_percent":92.5,"window_minutes":300,"resets_at":1700000000},"secondary":{"used_percent":40,"window_minutes":10080,"resets_at":1700600000}}}}"#,
        ]);
        let plan = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::PlanUsage { usage } => Some(usage.clone()),
                _ => None,
            })
            .expect("the windows are read");
        assert_eq!(plan.plan, None);
        assert_eq!(
            plan.windows,
            vec![
                PlanWindow {
                    label: "5h".into(),
                    used_percent: 92.5,
                    resets_at: Some(1_700_000_000),
                },
                PlanWindow {
                    label: "week".into(),
                    used_percent: 40.0,
                    resets_at: Some(1_700_600_000),
                },
            ]
        );
        assert_eq!(plan.tightest().map(|w| w.label.as_str()), Some("5h"));
    }

    #[test]
    fn a_token_count_with_no_windows_reports_none_rather_than_an_empty_reading() {
        let (events, _) = parse(&[
            r#"{"id":"1","msg":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5}},"rate_limits":null}}"#,
        ]);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::PlanUsage { .. })),
            "nothing was reported, so nothing is claimed"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ContextUsage { .. })),
            "session totals without a context capacity are not a context reading"
        );
    }

    #[test]
    fn context_usage_accepts_the_app_server_spelling_without_guessing() {
        assert_eq!(
            context_usage_from(&serde_json::json!({
                "tokenUsage": {
                    "last": { "totalTokens": 48_000 },
                    "modelContextWindow": 192_000
                }
            })),
            Some(ContextUsage {
                used_tokens: 48_000,
                window_tokens: 192_000,
                can_compact: true,
            })
        );
        assert_eq!(
            context_usage_from(&serde_json::json!({
                "tokenUsage": { "last": { "totalTokens": 48_000 } }
            })),
            None
        );
    }

    #[test]
    fn the_older_stream_reports_current_context_separately_from_session_totals() {
        let (events, _) = parse(&[
            r#"{"id":"1","msg":{"type":"token_count","info":{"total_token_usage":{"input_tokens":90000,"output_tokens":10000},"last_token_usage":{"total_tokens":32000},"model_context_window":128000}}}"#,
        ]);

        assert!(events.contains(&AgentEvent::ContextUsage {
            usage: ContextUsage {
                used_tokens: 32_000,
                window_tokens: 128_000,
                can_compact: true,
            },
        }));
        assert!(events.contains(&AgentEvent::Usage {
            usage: Usage {
                input_tokens: 90_000,
                output_tokens: 10_000,
                ..Usage::default()
            },
        }));
    }

    #[test]
    fn the_app_server_is_asked_for_the_windows_and_its_answer_is_read() {
        // The exchange and the answer as 0.142.5 gave them; the shape is the
        // fixture, not a live server (R6).
        let driver = CodexDriver::default();
        let probe = driver.plan_usage_probe().expect("codex can be asked");
        assert_eq!(probe.command.args, vec!["app-server"]);
        assert!(probe.input[0].contains("\"initialize\""));
        assert!(probe.input[2].contains("account/rateLimits/read"));

        assert_eq!(
            driver.parse_plan_usage(
                r#"{"id":1,"result":{"userAgent":"ginka/0.142.5","codexHome":"/Users/x/.codex"}}"#
            ),
            None,
            "the handshake's answer is not the reading"
        );
        assert_eq!(
            driver.parse_plan_usage(r#"{"method":"remoteControl/status/changed","params":{}}"#),
            None
        );
        let usage = driver
            .parse_plan_usage(
                r#"{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":1789141311},"secondary":null,"planType":"pro"}}}"#,
            )
            .expect("the reading");
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        assert_eq!(
            usage.windows,
            vec![PlanWindow {
                label: "week".into(),
                used_percent: 40.0,
                resets_at: Some(1_789_141_311),
            }]
        );
    }

    #[test]
    fn a_window_is_named_by_its_length() {
        assert_eq!(window_label(Some(300), "primary"), "5h");
        assert_eq!(window_label(Some(10_080), "secondary"), "week");
        assert_eq!(window_label(Some(2_880), "x"), "2d");
        assert_eq!(window_label(Some(90), "x"), "90m");
        assert_eq!(window_label(None, "primary"), "primary");
    }

    #[test]
    fn the_cli_is_pointed_at_an_accounts_directory_through_codex_home() {
        let driver = CodexDriver::default();
        assert_eq!(driver.home_variable(), Some("CODEX_HOME"));
        assert_eq!(driver.login_command().unwrap().args, vec!["login"]);
    }

    #[test]
    fn an_unknown_line_is_counted_so_a_format_change_is_visible() {
        let (events, state) = parse(&[r#"{"type":"something.new","payload":{}}"#]);
        assert!(events.is_empty());
        assert_eq!(state.unrecognized, 1);
        assert!(state.understood_nothing());
    }
}
