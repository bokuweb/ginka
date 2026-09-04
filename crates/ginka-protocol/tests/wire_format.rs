//! The wire contract, pinned.
//!
//! Every process in the system — and, once `export_types` runs, every future
//! non-Rust client — reads these shapes. A field rename here is a breaking
//! change for a daemon and a UI that ship separately, so the JSON is asserted
//! literally rather than only round-tripped.

use ginka_protocol::event::ActivityItem;
use ginka_protocol::event::{AgentEvent, DaemonEvent, Usage};
use ginka_protocol::model::{ProjectKind, SessionState};
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
fn a_response_is_tagged_by_result() {
    assert_eq!(wire(&Response::Ack), json!({ "result": "ack" }));
}

#[test]
fn a_request_round_trips_through_its_envelope() {
    let message = ClientMessage::Request {
        id: 7,
        payload: Request::Ping,
    };
    let text = serde_json::to_string(&message).unwrap();
    let parsed: ClientMessage = serde_json::from_str(&text).unwrap();
    match parsed {
        ClientMessage::Request { id, payload } => {
            assert_eq!(id, 7);
            assert!(matches!(payload, Request::Ping));
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
