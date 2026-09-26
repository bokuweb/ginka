//! Codex turns over `codex app-server`, where Codex can ask before it acts.
//!
//! `codex exec` runs a turn to the end on its own: a command its approval
//! policy would ask about is refused, and nothing can be said back. The app
//! server is the same agent behind JSON-RPC 2.0 on stdio, and it *asks* —
//! `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`
//! and `item/tool/requestUserInput` arrive as requests the client answers.
//! Each becomes the card the transcript already draws, and the reader's
//! choice goes back as that request's result.
//!
//! A turn is one process, as with every driver: `initialize`, then
//! `thread/start` (or `thread/resume` for a conversation Codex already has),
//! then one `turn/start` once the thread's id is known, whose
//! `turn/completed` ends the turn. The shapes are pinned by lines recorded
//! from Codex 0.154 (`tests` below) and checked against the protocol's own
//! JSON Schema (`codex app-server generate-json-schema`).
//!
//! The server marks itself experimental, so [`super::codex::CodexDriver`]
//! keeps `codex exec` as the transport a setting can fall back to.

use super::{ActivityItem, ParseState, SessionSpec};
use ginka_protocol::AgentEvent;
use ginka_protocol::model::SessionState;
use ginka_protocol::provider::AccessMode;
use ginka_protocol::{ContextUsage, Usage};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// JSON-RPC's "method not found", for requests this client does not serve.
const METHOD_NOT_FOUND: i64 = -32601;

/// The choice that lets a command or an edit go ahead once.
pub const ALLOW: &str = "Allow";
/// The choice that lets it, and the same again, go ahead for the session.
pub const ALLOW_SESSION: &str = "Allow for this session";
/// The choice that refuses it; the agent carries on without it.
pub const DENY: &str = "Deny";

/// How long a command is shown in a card before it is cut.
const SHOWN_CHARS: usize = 400;

/// This transport's half of one turn's exchange.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppState {
    /// Set by [`begin`]: the lines that follow are the app server's.
    pub active: bool,
    prompt: String,
    cwd: String,
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<String>,
    access: AccessMode,
    /// The thread to continue, when this turn continues one.
    resume: Option<String>,
    next_id: u64,
    /// The `thread/start` or `thread/resume` in flight.
    thread: Option<u64>,
    /// Whether [`AppState::thread`] is a resume, which falls back to a new
    /// thread when Codex no longer has the old one.
    resuming: bool,
    /// The `turn/start` in flight.
    turn: Option<u64>,
    /// Messages whose text already arrived as deltas, so their completion
    /// does not say it twice.
    streamed: BTreeSet<String>,
    /// The files each edit touches, from when it started, for its card.
    files: BTreeMap<String, Vec<String>>,
}

impl AppState {
    fn request(&mut self, method: &str, params: Value) -> (u64, String) {
        self.next_id += 1;
        let id = self.next_id;
        (
            id,
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        )
    }

    /// Open the thread: continue the old one when there is one.
    fn open(&mut self) -> String {
        let (sandbox, approval) = policy(self.access);
        let mut params = json!({
            "cwd": self.cwd,
            "sandbox": sandbox,
            "approvalPolicy": approval,
        });
        if let Some(model) = &self.model {
            params["model"] = json!(model);
        }
        if let Some(tier) = &self.service_tier {
            params["serviceTier"] = json!(tier);
        }
        let method = match &self.resume {
            Some(thread) => {
                params["threadId"] = json!(thread);
                self.resuming = true;
                "thread/resume"
            }
            None => {
                self.resuming = false;
                "thread/start"
            }
        };
        let (id, line) = self.request(method, params);
        self.thread = Some(id);
        line
    }

    /// Send the prompt into the thread that is now open.
    fn start_turn(&mut self, thread: &str) -> String {
        let mut params = json!({
            "threadId": thread,
            "input": [{"type": "text", "text": self.prompt}],
        });
        if let Some(effort) = &self.effort {
            params["effort"] = json!(effort);
        }
        let (id, line) = self.request("turn/start", params);
        self.turn = Some(id);
        line
    }
}

/// The sandbox and approval policy an access mode means to Codex.
///
/// `ask` edits the worktree freely and asks about everything that is not
/// plainly safe to run; `read-only` asks when the agent wants more than
/// reading; `auto` never asks.
pub fn policy(access: AccessMode) -> (&'static str, &'static str) {
    match access {
        AccessMode::ReadOnly => ("read-only", "on-request"),
        AccessMode::Ask => ("workspace-write", "untrusted"),
        AccessMode::Auto => ("workspace-write", "never"),
    }
}

/// The lines that open a turn, with its state set up for what comes back.
pub fn begin(spec: &SessionSpec, resume: Option<&str>, state: &mut ParseState) -> Vec<String> {
    let app = &mut state.codex;
    *app = AppState {
        active: true,
        prompt: spec.agent_prompt(),
        cwd: spec.workspace_path.display().to_string(),
        model: spec.model.clone(),
        effort: spec.reasoning_effort.clone(),
        service_tier: spec.service_tier.clone(),
        access: spec.access_mode,
        resume: resume.map(str::to_string),
        ..AppState::default()
    };
    let (_, initialize) = app.request(
        "initialize",
        json!({"clientInfo": {"name": "ginka", "title": "Ginka", "version": env!("CARGO_PKG_VERSION")}}),
    );
    let initialized = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}).to_string();
    // The server takes requests in order, so the thread can be asked for
    // before `initialize` has been answered.
    let open = app.open();
    vec![initialize, initialized, open]
}

/// One line from the app server.
pub fn parse(value: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    if value.get("jsonrpc").is_none() && value.get("id").is_none() && value.get("method").is_none()
    {
        state.unrecognized += 1;
        return Vec::new();
    }
    state.recognized += 1;
    let method = value.get("method").and_then(Value::as_str);
    let id = value.get("id").filter(|id| !id.is_null());
    match (method, id) {
        (None, Some(id)) => answer(id.as_u64(), value, state),
        (Some(method), Some(id)) => server_request(method, id, &value["params"], state),
        (Some(method), None) => notification(method, &value["params"], state),
        (None, None) => Vec::new(),
    }
}

/// The result of one of this client's own requests.
fn answer(id: Option<u64>, value: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let error = value.get("error").map(|error| {
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex refused the request")
            .to_string()
    });
    let mine = |asked: Option<u64>| id.is_some() && id == asked;
    if mine(state.codex.thread) {
        if let Some(error) = error {
            if !state.codex.resuming {
                return failed(state, error);
            }
            // A thread Codex no longer has: start a new one with what the
            // prompt says rather than fail the turn.
            state.codex.resume = None;
            state.vendor_session_id = None;
            let line = state.codex.open();
            state.outbox.push(line);
            return Vec::new();
        }
        let Some(thread) = value
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return failed(state, "Codex opened no thread".to_string());
        };
        state.vendor_session_id = Some(thread.clone());
        let line = state.codex.start_turn(&thread);
        state.outbox.push(line);
        return vec![AgentEvent::Connected {
            session_id: Some(thread),
            model: value
                .pointer("/result/model")
                .and_then(Value::as_str)
                .map(str::to_string),
        }];
    }
    if mine(state.codex.turn)
        && let Some(error) = error
    {
        return failed(state, error);
    }
    Vec::new()
}

/// A turn that cannot go on: closed, and said why.
fn failed(state: &mut ParseState, summary: String) -> Vec<AgentEvent> {
    state.turn += 1;
    vec![
        AgentEvent::TurnEnd { turn: state.turn },
        AgentEvent::SessionResult {
            state: SessionState::Failed,
            summary: Some(summary),
        },
    ]
}

/// Codex asking the client something: a card, or a refusal for what this
/// client never offered.
fn server_request(
    method: &str,
    id: &Value,
    params: &Value,
    state: &mut ParseState,
) -> Vec<AgentEvent> {
    let field = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default();
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty());
    let card = |kind: &str, extra: Value| {
        let mut card = json!({"rpc": id, "kind": kind});
        if let (Some(card), Some(extra)) = (card.as_object_mut(), extra.as_object()) {
            card.extend(extra.clone());
        }
        card.to_string()
    };
    let with_reason = |text: String| match reason {
        Some(reason) => format!("{text}\n{reason}"),
        None => text,
    };
    match method {
        "item/commandExecution/requestApproval" => vec![AgentEvent::AskUser {
            id: card("command", json!({})),
            question: with_reason(format!(
                "Codex wants to run:\n{}",
                cut(&asked_command(params))
            )),
            options: vec![ALLOW.into(), ALLOW_SESSION.into(), DENY.into()],
        }],
        "item/fileChange/requestApproval" => {
            let files = state
                .codex
                .files
                .get(field("itemId"))
                .map(|files| files.join("\n"))
                .filter(|files| !files.is_empty())
                .or_else(|| {
                    params
                        .get("grantRoot")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "files in the workspace".to_string());
            vec![AgentEvent::AskUser {
                id: card("file", json!({})),
                question: with_reason(format!("Codex wants to change:\n{files}")),
                options: vec![ALLOW.into(), ALLOW_SESSION.into(), DENY.into()],
            }]
        }
        "item/tool/requestUserInput" => {
            let questions: Vec<&Value> = params
                .get("questions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect();
            let ids: Vec<&str> = questions
                .iter()
                .filter_map(|question| question.get("id").and_then(Value::as_str))
                .collect();
            let text = |question: &Value| {
                question
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let (question, options) = match questions.as_slice() {
                [only] => (
                    text(only),
                    only.get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|option| option.get("label").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect(),
                ),
                several => (
                    several
                        .iter()
                        .map(|question| text(question))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Vec::new(),
                ),
            };
            vec![AgentEvent::AskUser {
                id: card("input", json!({"questions": ids})),
                question,
                options,
            }]
        }
        _ => {
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
    }
}

/// The line that answers a card, from the id it carried and what was chosen.
pub fn response(card: &str, answer: &str) -> Option<String> {
    let card: Value = serde_json::from_str(card).ok()?;
    let id = card.get("rpc")?.clone();
    let answer = answer.trim();
    let result = match card.get("kind")?.as_str()? {
        "command" | "file" => json!({"decision": match answer {
            ALLOW => "accept",
            ALLOW_SESSION => "acceptForSession",
            // Anything else refuses: a stale or odd answer must never let a
            // command run.
            _ => "decline",
        }}),
        "input" => {
            let answers: serde_json::Map<String, Value> = card
                .get("questions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|question| (question.to_string(), json!({"answers": [answer]})))
                .collect();
            json!({"answers": answers})
        }
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string())
}

/// Something the server reports as the turn goes.
fn notification(method: &str, params: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    match method {
        "item/agentMessage/delta" => {
            let Some(text) = params.get("delta").and_then(Value::as_str) else {
                return Vec::new();
            };
            if let Some(item) = params.get("itemId").and_then(Value::as_str) {
                state.codex.streamed.insert(item.to_string());
            }
            state.streaming = true;
            vec![AgentEvent::TextDelta {
                text: text.to_string(),
            }]
        }
        "item/started" => params
            .get("item")
            .map(|item| started(item, state))
            .unwrap_or_default(),
        "item/completed" => params
            .get("item")
            .map(|item| completed(item, state))
            .unwrap_or_default(),
        "turn/plan/updated" => plan(params),
        "thread/tokenUsage/updated" => {
            let usage = params.get("tokenUsage").unwrap_or(&Value::Null);
            let mut events = vec![AgentEvent::Usage {
                usage: usage_of(usage.get("total").unwrap_or(&Value::Null)),
            }];
            if let Some(usage) = super::codex::context_usage_from(params) {
                events.push(AgentEvent::ContextUsage {
                    usage: ContextUsage {
                        can_compact: true,
                        ..usage
                    },
                });
            }
            events
        }
        "account/rateLimits/updated" => params
            .get("rateLimits")
            .and_then(super::codex::plan_usage_from)
            .map(|usage| vec![AgentEvent::PlanUsage { usage }])
            .unwrap_or_default(),
        "turn/completed" => {
            let turn = params.get("turn").unwrap_or(&Value::Null);
            let (result, summary) = match turn.get("status").and_then(Value::as_str) {
                Some("failed") => (
                    SessionState::Failed,
                    turn.pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
                Some("interrupted") => (SessionState::Cancelled, None),
                _ => (SessionState::Finished, None),
            };
            state.turn += 1;
            vec![
                AgentEvent::TurnEnd { turn: state.turn },
                AgentEvent::SessionResult {
                    state: result,
                    summary,
                },
            ]
        }
        _ => Vec::new(),
    }
}

/// An item as it begins: tools show as running.
fn started(item: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let id = item.get("id").and_then(Value::as_str).map(str::to_string);
    match item.get("type").and_then(Value::as_str) {
        Some("commandExecution") => vec![AgentEvent::ToolCall {
            activity: command_activity(item, id),
        }],
        Some("fileChange") => {
            let files = changed_files(item);
            if let Some(id) = &id {
                state.codex.files.insert(id.clone(), files.clone());
            }
            vec![AgentEvent::ToolCall {
                activity: file_activity(id, &files),
            }]
        }
        Some("mcpToolCall") => vec![AgentEvent::ToolCall {
            activity: mcp_activity(item, id),
        }],
        _ => Vec::new(),
    }
}

/// An item as it ends: what was said, thought or done.
fn completed(item: &Value, state: &mut ParseState) -> Vec<AgentEvent> {
    let id = item.get("id").and_then(Value::as_str).map(str::to_string);
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
    let status_failed = matches!(
        item.get("status").and_then(Value::as_str),
        Some("failed" | "declined")
    );
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let streamed = id
                .as_ref()
                .is_some_and(|id| state.codex.streamed.remove(id));
            if streamed || text("text").is_empty() {
                Vec::new()
            } else {
                vec![AgentEvent::TextDelta {
                    text: text("text").to_string(),
                }]
            }
        }
        Some("plan") if !text("text").is_empty() => vec![AgentEvent::TextDelta {
            text: text("text").to_string(),
        }],
        Some("reasoning") => {
            let joined = ["summary", "content"]
                .iter()
                .filter_map(|key| item.get(*key).and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n\n");
            if joined.trim().is_empty() {
                Vec::new()
            } else {
                vec![AgentEvent::Reasoning { text: joined }]
            }
        }
        Some("commandExecution") => {
            let mut activity = command_activity(item, id);
            let failed = status_failed
                || item
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0);
            activity.complete_with(text("aggregatedOutput"), failed);
            vec![AgentEvent::ToolResult { activity }]
        }
        Some("fileChange") => {
            let files = changed_files(item);
            if let Some(id) = &id {
                state.codex.files.remove(id);
            }
            let mut activity = file_activity(id, &files);
            activity.complete_with(&files.join("\n"), status_failed);
            vec![AgentEvent::ToolResult { activity }]
        }
        Some("mcpToolCall") => {
            let mut activity = mcp_activity(item, id);
            let output = item
                .get("error")
                .filter(|error| !error.is_null())
                .or_else(|| item.get("result"))
                .map(|value| match value.as_str() {
                    Some(text) => text.to_string(),
                    None => value.to_string(),
                })
                .unwrap_or_default();
            activity.complete_with(
                &output,
                status_failed || item.get("error").is_some_and(|e| !e.is_null()),
            );
            vec![AgentEvent::ToolResult { activity }]
        }
        Some("webSearch") => {
            let mut activity =
                ActivityItem::from_tool(id, "web_search", &json!({"query": text("query")}));
            activity.complete_with("", false);
            vec![
                AgentEvent::ToolCall {
                    activity: activity.clone(),
                },
                AgentEvent::ToolResult { activity },
            ]
        }
        _ => Vec::new(),
    }
}

/// The command as it was asked for: the parsed actions, not the login-shell
/// wrapper (`/bin/zsh -lc '…'`) Codex runs them in.
fn asked_command(params: &Value) -> String {
    let actions: Vec<&str> = params
        .get("commandActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("command").and_then(Value::as_str))
        .collect();
    if actions.is_empty() {
        params
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        actions.join(" && ")
    }
}

fn command_activity(item: &Value, id: Option<String>) -> ActivityItem {
    // The parsed action is what was asked for; the raw command is wrapped
    // in the login shell that runs it.
    let command = item
        .pointer("/commandActions/0/command")
        .and_then(Value::as_str)
        .or_else(|| item.get("command").and_then(Value::as_str))
        .unwrap_or_default();
    ActivityItem::from_tool(id, "shell", &json!({"command": command}))
}

fn file_activity(id: Option<String>, files: &[String]) -> ActivityItem {
    ActivityItem::from_tool(
        id,
        "apply_patch",
        &json!({"path": files.first().cloned().unwrap_or_default()}),
    )
}

fn mcp_activity(item: &Value, id: Option<String>) -> ActivityItem {
    let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
    let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
    ActivityItem::from_tool(
        id,
        &format!("mcp__{server}__{tool}"),
        item.get("arguments").unwrap_or(&Value::Null),
    )
}

fn changed_files(item: &Value) -> Vec<String> {
    item.get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| change.get("path").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The turn's to-do list, as the one Tasks card every provider shares.
fn plan(params: &Value) -> Vec<AgentEvent> {
    let tasks: Vec<Value> = params
        .get("plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| {
            let text = step.get("step").and_then(Value::as_str)?;
            let status = match step.get("status").and_then(Value::as_str) {
                Some("completed") => "completed",
                Some("inProgress") => "in_progress",
                _ => "pending",
            };
            Some(json!({"text": text, "status": status}))
        })
        .collect();
    if tasks.is_empty() {
        return Vec::new();
    }
    let turn = params
        .get("turnId")
        .and_then(Value::as_str)
        .unwrap_or("turn");
    vec![AgentEvent::ToolCall {
        activity: ActivityItem::from_tool(
            Some(format!("plan-{turn}")),
            "update_plan",
            &json!({"tasks": tasks}),
        ),
    }]
}

/// The server's totals, in its camelCase.
fn usage_of(total: &Value) -> Usage {
    let number = |key: &str| total.get(key).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: number("inputTokens"),
        output_tokens: number("outputTokens"),
        cache_read_tokens: number("cachedInputTokens"),
        reasoning_tokens: number("reasoningOutputTokens"),
        // Codex does not price a run, so a cost here would be invented.
        cost_usd: None,
    }
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(SHOWN_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(access: AccessMode) -> SessionSpec {
        let mut spec = SessionSpec::new("/work/comet", "run the tests");
        spec.model = Some("gpt-5.6-luna".into());
        spec.access_mode = access;
        spec
    }

    fn sent(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    fn feed(state: &mut ParseState, line: &str) -> Vec<AgentEvent> {
        parse(&sent(line), state)
    }

    #[test]
    fn a_turn_opens_a_thread_under_the_access_modes_policy_before_anything_else() {
        let mut state = ParseState::default();
        let lines = begin(&spec(AccessMode::Ask), None, &mut state);
        let methods: Vec<String> = lines
            .iter()
            .map(|line| sent(line)["method"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(methods, ["initialize", "initialized", "thread/start"]);
        let open = sent(&lines[2]);
        assert_eq!(open["params"]["cwd"], "/work/comet");
        assert_eq!(open["params"]["sandbox"], "workspace-write");
        assert_eq!(open["params"]["approvalPolicy"], "untrusted");
        assert_eq!(open["params"]["model"], "gpt-5.6-luna");
        assert_eq!(policy(AccessMode::ReadOnly), ("read-only", "on-request"));
        assert_eq!(policy(AccessMode::Auto), ("workspace-write", "never"));
    }

    #[test]
    fn the_prompt_goes_once_the_thread_has_an_id_and_the_id_is_the_vendors() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::Ask), None, &mut state);
        let events = feed(
            &mut state,
            r#"{"id":2,"result":{"thread":{"id":"th-1"},"model":"gpt-5.6-luna"}}"#,
        );
        assert!(
            matches!(&events[..], [AgentEvent::Connected { session_id: Some(id), .. }] if id == "th-1")
        );
        assert_eq!(state.vendor_session_id.as_deref(), Some("th-1"));
        let turn = sent(&state.outbox.remove(0));
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "th-1");
        assert_eq!(turn["params"]["input"][0]["text"], "run the tests");
    }

    #[test]
    fn a_thread_codex_no_longer_has_is_started_again_rather_than_failed() {
        let mut state = ParseState::default();
        let lines = begin(&spec(AccessMode::Ask), Some("gone"), &mut state);
        assert_eq!(sent(&lines[2])["method"], "thread/resume");
        assert_eq!(sent(&lines[2])["params"]["threadId"], "gone");
        let events = feed(
            &mut state,
            r#"{"id":2,"error":{"code":-32600,"message":"thread not found"}}"#,
        );
        assert!(events.is_empty());
        let retry = sent(&state.outbox.remove(0));
        assert_eq!(retry["method"], "thread/start");
        let events = feed(&mut state, r#"{"id":3,"error":{"code":1,"message":"no"}}"#);
        assert!(matches!(
            &events[..],
            [
                AgentEvent::TurnEnd { .. },
                AgentEvent::SessionResult {
                    state: SessionState::Failed,
                    ..
                }
            ]
        ));
    }

    /// Recorded from Codex 0.154 running `echo ginka-probe` under
    /// `untrusted`, trimmed to the fields that matter.
    const APPROVAL: &str = r#"{"method":"item/commandExecution/requestApproval","id":0,"params":{"kind":"command","threadId":"th-1","turnId":"tu-1","itemId":"exec-1","startedAtMs":1790405850323,"environmentId":"local","command":"/bin/zsh -lc 'echo ginka-probe'","cwd":"/work/comet","commandActions":[{"type":"unknown","command":"echo ginka-probe"}],"proposedExecpolicyAmendment":["echo","ginka-probe"],"availableDecisions":["accept","acceptForSession","decline","cancel"]}}"#;

    #[test]
    fn a_command_approval_is_a_card_and_the_choice_is_its_decision() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::Ask), None, &mut state);
        let events = feed(&mut state, APPROVAL);
        let [
            AgentEvent::AskUser {
                id,
                question,
                options,
            },
        ] = events.as_slice()
        else {
            panic!("expected a card, got {events:?}");
        };
        assert!(question.contains("echo ginka-probe"), "{question}");
        assert!(
            !question.contains("/bin/zsh"),
            "the wrapper is not what was asked: {question}"
        );
        assert_eq!(options, &[ALLOW, ALLOW_SESSION, DENY]);
        let decision = |answer: &str| sent(&response(id, answer).unwrap());
        assert_eq!(decision(ALLOW)["id"], 0);
        assert_eq!(decision(ALLOW)["result"]["decision"], "accept");
        assert_eq!(
            decision(ALLOW_SESSION)["result"]["decision"],
            "acceptForSession"
        );
        assert_eq!(decision(DENY)["result"]["decision"], "decline");
        assert_eq!(decision("whatever")["result"]["decision"], "decline");
    }

    #[test]
    fn a_file_change_card_names_the_files_the_edit_touches() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::ReadOnly), None, &mut state);
        feed(
            &mut state,
            r#"{"method":"item/started","params":{"item":{"type":"fileChange","id":"fc-1","status":"inProgress","changes":[{"path":"src/lib.rs","kind":{"type":"update"},"diff":""}]}}}"#,
        );
        let events = feed(
            &mut state,
            r#"{"method":"item/fileChange/requestApproval","id":5,"params":{"threadId":"th-1","turnId":"tu-1","itemId":"fc-1","startedAtMs":1}}"#,
        );
        let [AgentEvent::AskUser { question, .. }] = events.as_slice() else {
            panic!("expected a card, got {events:?}");
        };
        assert!(question.contains("src/lib.rs"), "{question}");
    }

    #[test]
    fn the_agents_question_is_answered_by_question_id() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::Ask), None, &mut state);
        let events = feed(
            &mut state,
            r#"{"method":"item/tool/requestUserInput","id":7,"params":{"threadId":"th-1","turnId":"tu-1","itemId":"i","isBlocking":true,"questions":[{"id":"db","header":"DB","question":"Which database?","options":[{"label":"SQLite","description":"local"},{"label":"Postgres","description":"server"}]}]}}"#,
        );
        let [
            AgentEvent::AskUser {
                id,
                question,
                options,
            },
        ] = events.as_slice()
        else {
            panic!("expected a card, got {events:?}");
        };
        assert_eq!(question, "Which database?");
        assert_eq!(options, &["SQLite", "Postgres"]);
        let answered = sent(&response(id, "SQLite").unwrap());
        assert_eq!(answered["id"], 7);
        assert_eq!(answered["result"]["answers"]["db"]["answers"][0], "SQLite");
    }

    #[test]
    fn a_request_this_client_never_offered_is_refused_at_once() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::Ask), None, &mut state);
        let events = feed(
            &mut state,
            r#"{"method":"mcpServer/elicitation/request","id":9,"params":{}}"#,
        );
        assert!(events.is_empty());
        let refused = sent(&state.outbox.remove(0));
        assert_eq!(refused["id"], 9);
        assert_eq!(refused["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn streamed_text_is_not_said_twice_and_a_command_pairs_its_call_and_result() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::Ask), None, &mut state);
        let delta = feed(
            &mut state,
            r#"{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"ginka-"}}"#,
        );
        assert_eq!(
            delta,
            [AgentEvent::TextDelta {
                text: "ginka-".into()
            }]
        );
        let done = feed(
            &mut state,
            r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m1","text":"ginka-probe"}}}"#,
        );
        assert!(done.is_empty());
        let whole = feed(
            &mut state,
            r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m2","text":"unstreamed"}}}"#,
        );
        assert_eq!(
            whole,
            [AgentEvent::TextDelta {
                text: "unstreamed".into()
            }]
        );

        let call = feed(
            &mut state,
            r#"{"method":"item/started","params":{"item":{"type":"commandExecution","id":"exec-1","command":"/bin/zsh -lc 'echo hi'","commandActions":[{"type":"unknown","command":"echo hi"}],"cwd":"/w","status":"inProgress"}}}"#,
        );
        let [AgentEvent::ToolCall { activity }] = call.as_slice() else {
            panic!("{call:?}")
        };
        assert_eq!(activity.title, "echo hi");
        let result = feed(
            &mut state,
            r#"{"method":"item/completed","params":{"item":{"type":"commandExecution","id":"exec-1","command":"x","commandActions":[{"type":"unknown","command":"echo hi"}],"cwd":"/w","status":"completed","aggregatedOutput":"hi\n","exitCode":0}}}"#,
        );
        let [AgentEvent::ToolResult { activity }] = result.as_slice() else {
            panic!("{result:?}")
        };
        assert_eq!(activity.id.as_deref(), Some("exec-1"));
        assert!(activity.complete && !activity.failed);
    }

    #[test]
    fn the_turns_end_carries_its_outcome_and_usage_is_read_as_it_comes() {
        let mut state = ParseState::default();
        begin(&spec(AccessMode::Ask), None, &mut state);
        let usage = feed(
            &mut state,
            r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"th-1","turnId":"tu-1","tokenUsage":{"total":{"totalTokens":28040,"inputTokens":27904,"cachedInputTokens":24064,"outputTokens":136,"reasoningOutputTokens":28},"last":{"totalTokens":14038,"inputTokens":14030,"cachedInputTokens":13056,"outputTokens":8,"reasoningOutputTokens":0},"modelContextWindow":258400}}}"#,
        );
        assert!(usage.iter().any(|event| matches!(
            event,
            AgentEvent::Usage { usage } if usage.input_tokens == 27904 && usage.output_tokens == 136
        )));
        assert!(usage.iter().any(|event| matches!(
            event,
            AgentEvent::ContextUsage { usage } if usage.used_tokens == 14038 && usage.window_tokens == 258400
        )));
        let done = feed(
            &mut state,
            r#"{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"tu-1","status":"completed"}}}"#,
        );
        assert_eq!(
            done,
            [
                AgentEvent::TurnEnd { turn: 1 },
                AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    summary: None
                }
            ]
        );
        let failed = feed(
            &mut state,
            r#"{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"tu-2","status":"failed","error":{"message":"model not supported"}}}}"#,
        );
        assert!(matches!(
            &failed[1],
            AgentEvent::SessionResult { state: SessionState::Failed, summary: Some(summary) } if summary == "model not supported"
        ));
    }

    #[test]
    fn a_todo_list_becomes_the_shared_tasks_card() {
        let events = plan(&json!({"turnId": "tu-1", "plan": [
            {"step": "Read", "status": "completed"},
            {"step": "Fix", "status": "inProgress"},
        ]}));
        let [AgentEvent::ToolCall { activity }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(activity.tasks.as_ref().map(Vec::len), Some(2));
    }
}
