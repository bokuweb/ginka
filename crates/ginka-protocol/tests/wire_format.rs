//! The wire contract, pinned.
//!
//! Every process in the system — and, once `export_types` runs, every future
//! non-Rust client — reads these shapes. A field rename here is a breaking
//! change for a daemon and a UI that ship separately, so the JSON is asserted
//! literally rather than only round-tripped.

use ginka_protocol::event::{ActivityItem, SubagentStep, SubagentStepKind, SubagentStepStatus};
use ginka_protocol::event::{AgentEvent, ContextUsage, DaemonEvent, Usage};
use ginka_protocol::model::{
    AgentStatus, FileContent, FileImage, ProjectKind, SessionState, TranscriptPayload,
};
use ginka_protocol::provider::{ProviderModel, ProviderOption};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{ClientMessage, RpcError, ServerMessage};
use ginka_protocol::{ProjectName, SessionId, WorkspaceId};
use serde_json::{Value, json};

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serializes")
}

#[test]
fn a_request_is_tagged_by_method() {
    let request = Request::CreateWorkspace {
        project: ProjectName("comet".into()),
        branch: "bright-harbor".into(),
        base: None,
    };
    assert_eq!(
        wire(&request),
        json!({
            "method": "create_workspace",
            "project": "comet",
            "branch": "bright-harbor",
            "base": null,
        })
    );
}

#[test]
fn adding_a_project_keeps_its_optional_display_name_on_the_wire() {
    let request = Request::AddProject {
        path: "/work/comet".into(),
        label: Some("Client website".into()),
    };
    assert_eq!(
        wire(&request),
        json!({
            "method": "add_project",
            "path": "/work/comet",
            "label": "Client website",
        })
    );
    let old = json!({ "method": "add_project", "path": "/work/comet" });
    assert_eq!(
        serde_json::from_value::<Request>(old).unwrap(),
        Request::AddProject {
            path: "/work/comet".into(),
            label: None,
        }
    );
}

#[test]
fn workspace_archive_is_an_explicit_reversible_wire_operation() {
    let request = Request::ArchiveWorkspace {
        workspace: WorkspaceId("comet/bright-harbor".into()),
        archived: true,
    };
    assert_eq!(
        wire(&request),
        json!({
            "method": "archive_workspace",
            "workspace": "comet/bright-harbor",
            "archived": true,
        })
    );
}

#[test]
fn a_file_save_carries_the_revision_it_may_replace() {
    let request = Request::WriteFile {
        workspace: WorkspaceId("comet/bright-harbor".into()),
        path: "src/main.rs".into(),
        text: "fn main() {}\n".into(),
        expected_revision: "abc123".into(),
    };
    assert_eq!(
        wire(&request),
        json!({
            "method": "write_file",
            "workspace": "comet/bright-harbor",
            "path": "src/main.rs",
            "text": "fn main() {}\n",
            "expected_revision": "abc123",
        })
    );
}

#[test]
fn a_project_search_names_its_scope_and_global_limit() {
    let request = Request::SearchProject {
        project: ProjectName("comet".into()),
        query: "needle".into(),
        limit: Some(12),
    };
    assert_eq!(
        wire(&request),
        json!({
            "method": "search_project",
            "project": "comet",
            "query": "needle",
            "limit": 12,
        })
    );
}

#[test]
fn a_session_start_carries_provider_model_options_explicitly() {
    let request = Request::StartSession {
        workspace: WorkspaceId("comet/bright-harbor".into()),
        agent: "codex".into(),
        prompt: "go".into(),
        model: Some("gpt-next".into()),
        reasoning_effort: Some("high".into()),
        service_tier: Some("priority".into()),
        account: None,
        access_mode: None,
        origin: None,
    };
    let wired = wire(&request);
    assert_eq!(wired["reasoning_effort"], "high");
    assert_eq!(wired["service_tier"], "priority");
    assert_eq!(serde_json::from_value::<Request>(wired).unwrap(), request);
}

#[test]
fn an_existing_sessions_provider_options_cross_the_wire() {
    let request = Request::UpdateSessionOptions {
        session: SessionId("session-1".into()),
        model: Some("gpt-next".into()),
        reasoning_effort: Some("high".into()),
        service_tier: Some("priority".into()),
    };
    let wired = serde_json::to_value(&request).unwrap();
    assert_eq!(wired["method"], "update_session_options");
    assert_eq!(wired["model"], "gpt-next");
    assert_eq!(wired["reasoning_effort"], "high");
    assert_eq!(wired["service_tier"], "priority");
}

#[test]
fn a_response_is_tagged_by_result() {
    assert_eq!(wire(&Response::Ack), json!({ "result": "ack" }));
}

#[test]
fn a_file_image_crosses_the_shared_wire_as_bounded_base64() {
    let response = Response::FileContent {
        file: FileContent {
            path: "logo.png".into(),
            text: String::new(),
            revision: "rev-1".into(),
            binary: true,
            truncated: false,
            image: Some(FileImage {
                media_type: "image/png".into(),
                data_base64: "iVBORw0KGgo=".into(),
            }),
        },
    };

    assert_eq!(
        wire(&response),
        json!({
            "result": "file_content",
            "file": {
                "path": "logo.png",
                "text": "",
                "revision": "rev-1",
                "binary": true,
                "truncated": false,
                "image": {
                    "media_type": "image/png",
                    "data_base64": "iVBORw0KGgo="
                }
            }
        })
    );
}

#[test]
fn an_agent_catalogue_keeps_the_options_each_model_accepts() {
    let model = ProviderModel::new("gpt-next", "GPT Next")
        .as_default()
        .with_reasoning_efforts([ProviderOption::new("high", "High")])
        .with_service_tiers([ProviderOption::new("priority", "Fast")]);
    let response = Response::Agents {
        agents: vec![AgentStatus {
            id: "codex".into(),
            display_name: "Codex".into(),
            program: "codex".into(),
            installed: true,
            version: Some("1.0".into()),
            authenticated: Some(true),
            detail: None,
            models: vec![model],
        }],
    };

    let wired = wire(&response);
    assert_eq!(wired["agents"][0]["models"][0]["id"], "gpt-next");
    assert_eq!(
        wired["agents"][0]["models"][0]["reasoning_efforts"][0]["id"],
        "high"
    );
    assert_eq!(
        wired["agents"][0]["models"][0]["service_tiers"][0]["id"],
        "priority"
    );
}

#[test]
fn a_request_round_trips_through_its_envelope() {
    let message = ClientMessage::Request {
        id: 7,
        payload: Box::new(Request::Ping),
    };
    let text = serde_json::to_string(&message).unwrap();
    let parsed: ClientMessage = serde_json::from_str(&text).unwrap();
    match parsed {
        ClientMessage::Request { id, payload } => {
            assert_eq!(id, 7);
            assert!(matches!(*payload, Request::Ping));
        }
        other => panic!("expected a request, got {other:?}"),
    }
}

#[test]
fn a_failed_request_answers_with_an_error_not_a_wrapped_result() {
    // `Result` serialises as {"Ok":…}/{"Err":…}, which is a Rust detail no
    // other client should have to know about. Failure is its own message.
    let message = ServerMessage::Error {
        id: 3,
        error: RpcError {
            code: "not_found".into(),
            message: "no project named comet".into(),
        },
    };
    assert_eq!(
        wire(&message),
        json!({
            "type": "error",
            "id": 3,
            "error": { "code": "not_found", "message": "no project named comet" },
        })
    );
}

#[test]
fn every_push_carries_a_sequence_number_so_gaps_are_detectable() {
    let message = ServerMessage::Event {
        seq: 42,
        payload: DaemonEvent::WorkspacesChanged {
            project: ProjectName("comet".into()),
        },
    };
    let wired = wire(&message);
    assert_eq!(wired["type"], json!("event"));
    assert_eq!(wired["seq"], json!(42));
    assert_eq!(wired["payload"]["event"], json!("workspaces_changed"));
}

#[test]
fn agent_events_are_tagged_by_kind() {
    let cases: Vec<(AgentEvent, &str)> = vec![
        (
            AgentEvent::TextDelta {
                text: "hello".into(),
            },
            "text_delta",
        ),
        (
            AgentEvent::Reasoning {
                text: "thinking".into(),
            },
            "reasoning",
        ),
        (
            AgentEvent::ToolCall {
                activity: ActivityItem::from_tool(
                    Some("call_1".into()),
                    "read_file",
                    &json!({ "path": "src/main.rs" }),
                ),
            },
            "tool_call",
        ),
        (
            AgentEvent::ToolResult {
                activity: {
                    let mut activity = ActivityItem::from_tool(
                        Some("call_1".into()),
                        "read_file",
                        &json!({ "path": "src/main.rs" }),
                    );
                    activity.complete_with("fn main() {}", false);
                    activity
                },
            },
            "tool_result",
        ),
        (
            AgentEvent::SubagentStarted {
                id: "agent_1".into(),
                title: "Review correctness".into(),
            },
            "subagent_started",
        ),
        (
            AgentEvent::SubagentStep {
                parent_id: "agent_1".into(),
                step: SubagentStep::new("read_1", SubagentStepKind::Tool, "Read src/lib.rs")
                    .with_status(SubagentStepStatus::Running),
            },
            "subagent_step",
        ),
        (
            AgentEvent::SubagentFinished {
                id: "agent_1".into(),
                summary: Some("No regressions".into()),
                failed: false,
            },
            "subagent_finished",
        ),
        (
            AgentEvent::AskUser {
                id: "ask_1".into(),
                question: "Which database?".into(),
                options: vec!["sqlite".into(), "postgres".into()],
            },
            "ask_user",
        ),
        (
            AgentEvent::PlanProposal {
                id: "plan_1".into(),
                plan: "1. write the test".into(),
            },
            "plan_proposal",
        ),
        (
            AgentEvent::Permission {
                id: "permission_1".into(),
                request: "Run the test suite?".into(),
            },
            "permission",
        ),
        (
            AgentEvent::Usage {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cache_read_tokens: 0,
                    reasoning_tokens: 0,
                    cost_usd: Some(0.001),
                },
            },
            "usage",
        ),
        (
            AgentEvent::ContextUsage {
                usage: ContextUsage {
                    used_tokens: 32_000,
                    window_tokens: 128_000,
                    can_compact: true,
                },
            },
            "context_usage",
        ),
        (AgentEvent::TurnEnd { turn: 1 }, "turn_end"),
        (
            AgentEvent::SessionResult {
                state: SessionState::Finished,
                summary: Some("done".into()),
            },
            "session_result",
        ),
    ];

    for (event, kind) in cases {
        let wired = wire(&event);
        assert_eq!(wired["kind"], json!(kind), "{event:?}");
        let parsed: AgentEvent = serde_json::from_value(wired).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
    }
}

#[test]
fn an_interaction_response_keeps_the_request_it_answers() {
    assert_eq!(
        wire(&TranscriptPayload::Response {
            request_id: "ask_1".into(),
            text: "SQLite".into(),
        }),
        json!({
            "source": "response",
            "request_id": "ask_1",
            "text": "SQLite",
        })
    );
}

#[test]
fn enums_the_ui_switches_on_are_snake_case_on_the_wire() {
    assert_eq!(wire(&ProjectKind::Plain), json!("plain"));
    assert_eq!(wire(&SessionState::AwaitingInput), json!("awaiting_input"));
}

#[test]
fn ids_are_transparent_strings_rather_than_wrapper_objects() {
    assert_eq!(
        wire(&WorkspaceId("comet/harbor".into())),
        json!("comet/harbor")
    );
    assert_eq!(wire(&SessionId("s-1".into())), json!("s-1"));
}

#[test]
fn an_unknown_method_is_a_parse_error_rather_than_a_silent_default() {
    // A daemon that is older than its client must fail loudly on a method it
    // does not implement; silently defaulting would drop the request.
    let parsed = serde_json::from_str::<Request>(r#"{"method":"teleport"}"#);
    assert!(parsed.is_err());
}

#[test]
fn a_commit_is_named_as_a_change_source_and_a_note_as_a_request() {
    assert_eq!(
        wire(&ginka_protocol::model::ChangeSource::Commit {
            commit: "abc123".into()
        }),
        json!({"against": "commit", "commit": "abc123"})
    );
    assert_eq!(
        wire(&Request::SaveNote {
            id: None,
            project: None,
            title: "t".into(),
            body: "b".into(),
        }),
        json!({"method": "save_note", "id": null, "project": null, "title": "t", "body": "b"})
    );
    assert_eq!(
        wire(&Request::CreatePullRequest {
            workspace: WorkspaceId("comet/harbor".into()),
            draft: false,
        }),
        json!({"method": "create_pull_request", "workspace": "comet/harbor", "draft": false})
    );
}

#[test]
fn an_edited_prompt_names_its_session_position_and_text() {
    assert_eq!(
        wire(&Request::EditPrompt {
            session: SessionId("s-1".into()),
            seq: 4,
            text: "again".into(),
        }),
        json!({"method": "edit_prompt", "session": "s-1", "seq": 4, "text": "again"})
    );
}

#[test]
fn a_merge_names_the_workspace_and_optionally_its_target_and_message() {
    assert_eq!(
        wire(&Request::MergeWorkspace {
            workspace: WorkspaceId("comet/try-1".into()),
            into: None,
            message: Some("Keep try-1".into()),
        }),
        json!({"method": "merge_workspace", "workspace": "comet/try-1", "into": null, "message": "Keep try-1"})
    );
}

#[test]
fn a_scheduled_job_says_how_it_runs_in_words() {
    assert_eq!(
        wire(&Request::SaveCronJob {
            id: None,
            project: ginka_protocol::ProjectName("comet".into()),
            workspace: None,
            name: "nightly".into(),
            schedule: "@daily".into(),
            via: ginka_protocol::model::CronVia::Terminal,
            agent: None,
            body: "cargo test".into(),
            enabled: true,
        }),
        json!({
            "method": "save_cron_job", "id": null, "project": "comet", "workspace": null,
            "name": "nightly", "schedule": "@daily", "via": "terminal", "agent": null,
            "body": "cargo test", "enabled": true
        })
    );
}

#[test]
fn a_project_is_moved_by_name_to_an_index() {
    assert_eq!(
        wire(&Request::MoveProject {
            project: ginka_protocol::ProjectName("comet".into()),
            index: 2,
        }),
        json!({"method": "move_project", "project": "comet", "index": 2})
    );
}

#[test]
fn installing_ginkas_skills_says_whether_to_replace_an_edit() {
    assert_eq!(
        wire(&Request::InstallBundledSkills { force: false }),
        json!({"method": "install_bundled_skills", "force": false})
    );
}

#[test]
fn a_browser_visit_names_its_workspace_url_and_title() {
    assert_eq!(
        wire(&Request::RecordBrowserVisit {
            workspace: WorkspaceId("comet/main".into()),
            url: "http://localhost:3000/".into(),
            title: None,
        }),
        json!({"method": "record_browser_visit", "workspace": "comet/main", "url": "http://localhost:3000/", "title": null})
    );
}

#[test]
fn a_setting_is_changed_by_its_key_and_a_json_value() {
    assert_eq!(
        wire(&Request::UpdateDaemonSettings {
            key: "keep_awake".into(),
            value: "false".into(),
        }),
        json!({"method": "update_daemon_settings", "key": "keep_awake", "value": "false"})
    );
}
