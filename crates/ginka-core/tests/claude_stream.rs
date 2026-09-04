//! Claude Code's stream-json, normalized into the one event stream.
//!
//! Recorded shapes rather than a live CLI: vendors change these without
//! notice, and a fixture is the only thing that tells us *which* line stopped
//! being understood (roadmap R6).

use ginka_core::driver::{ActivityKind, AgentEvent, ClaudeStream, DriverError};
use ginka_protocol::model::SessionState;

fn events(lines: &[&str]) -> Vec<AgentEvent> {
    let mut stream = ClaudeStream::default();
    lines
        .iter()
        .flat_map(|line| stream.push_line(line).unwrap())
        .collect()
}

const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-sonnet-4-5","slash_commands":["compact","review"],"tools":["Bash","Edit"]}"#;

#[test]
fn the_init_line_connects_the_session_and_reports_its_commands() {
    let events = events(&[INIT]);
    assert_eq!(
        events[0],
        AgentEvent::Connected {
            session_id: Some("sess-1".into()),
            model: Some("claude-sonnet-4-5".into()),
        }
    );
    match &events[1] {
        AgentEvent::Commands { commands } => assert_eq!(commands, &["compact", "review"]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_session_id_is_kept_for_resuming_later() {
    let mut stream = ClaudeStream::default();
    stream.push_line(INIT).unwrap();
    assert_eq!(stream.session_id(), Some("sess-1"));
}

#[test]
fn assistant_text_becomes_a_delta() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking at the diff."}]}}"#,
    ]);
    assert_eq!(
        events,
        [AgentEvent::TextDelta {
            text: "Looking at the diff.".into()
        }]
    );
}

#[test]
fn thinking_is_reasoning_and_never_text() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"weighing two options"}]}}"#,
    ]);
    assert_eq!(
        events,
        [AgentEvent::Reasoning {
            text: "weighing two options".into()
        }]
    );
}

#[test]
fn a_tool_use_becomes_a_normalized_call() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
    ]);
    match &events[0] {
        AgentEvent::ToolCall { activity } => {
            assert_eq!(activity.kind, ActivityKind::Command);
            assert_eq!(activity.title, "cargo test");
            assert_eq!(activity.id.as_deref(), Some("t1"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_result_completes_the_call_it_names_and_keeps_that_calls_title() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok. 220 passed","is_error":false}]}}"#,
    ]);
    match &events[1] {
        AgentEvent::ToolResult { activity } => {
            assert_eq!(activity.title, "cargo test", "the row keeps its identity");
            assert_eq!(activity.kind, ActivityKind::Command);
            assert_eq!(activity.detail.as_deref(), Some("ok. 220 passed"));
            assert!(activity.complete);
            assert!(!activity.failed);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failed_tool_result_says_so() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"false"}}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"exit 1","is_error":true}]}}"#,
    ]);
    match &events[1] {
        AgentEvent::ToolResult { activity } => assert!(activity.failed),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_result_for_a_call_we_never_saw_is_still_shown() {
    // A resumed session replays results whose calls happened before we
    // attached; dropping them would leave the transcript lying.
    let events = events(&[
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"orphan","content":"done"}]}}"#,
    ]);
    match &events[0] {
        AgentEvent::ToolResult { activity } => {
            assert_eq!(activity.id.as_deref(), Some("orphan"));
            assert!(activity.complete);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn structured_tool_output_is_flattened_into_readable_text() {
    let events = events(&[
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"line one"},{"type":"text","text":"line two"}]}]}}"#,
    ]);
    match &events[0] {
        AgentEvent::ToolResult { activity } => {
            assert_eq!(activity.detail.as_deref(), Some("line one\nline two"))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn usage_is_reported_with_its_cache_split_intact() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":7}}}"#,
    ]);
    match events.last().unwrap() {
        AgentEvent::Usage { usage } => {
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 20);
            // One cache figure on the wire: what was read and what was written
            // are both input the vendor charged differently.
            assert_eq!(usage.cache_read_tokens, 12);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_result_line_ends_the_turn() {
    let events = events(&[
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","session_id":"sess-1","usage":{"input_tokens":1,"output_tokens":2}}"#,
    ]);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Usage { .. }))
    );
    // The turn ends, and the session's own result follows it.
    assert!(events.contains(&AgentEvent::TurnEnd { turn: 1 }));
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::SessionResult { .. }
    ));
}

#[test]
fn a_failed_turn_carries_the_reason_the_agent_gave() {
    let events = events(&[
        r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"hit the turn limit"}"#,
    ]);
    match events.last().unwrap() {
        AgentEvent::SessionResult { state, summary } => {
            assert_eq!(*state, SessionState::Failed);
            let summary = summary.clone().unwrap_or_default();
            assert!(
                summary.contains("max_turns") || summary.contains("turn limit"),
                "{summary}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn partial_deltas_stream_before_the_message_is_complete() {
    let events = events(&[
        r#"{"type":"stream_event","event":{"type":"message_start"}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"par"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}}"#,
    ]);
    assert_eq!(
        events,
        [
            AgentEvent::TurnStarted,
            AgentEvent::TextDelta { text: "par".into() },
            AgentEvent::Reasoning { text: "hmm".into() },
        ]
    );
}

#[test]
fn a_blank_line_produces_nothing() {
    assert!(events(&["", "   ", "\n"]).is_empty());
}

#[test]
fn a_shape_this_build_does_not_know_is_surfaced_rather_than_guessed_at() {
    // Loud, but not fatal: a vendor adding a message type should show up in
    // the transcript as "not understood", not end the session and not be
    // silently mis-parsed into something it is not.
    let events = events(&[r#"{"type":"telemetry_ping","value":1}"#]);
    assert_eq!(
        events,
        [AgentEvent::Unsupported {
            shape: "telemetry_ping".into()
        }]
    );
}

#[test]
fn an_unknown_content_block_is_surfaced_the_same_way() {
    let events =
        events(&[r#"{"type":"assistant","message":{"content":[{"type":"video","url":"x"}]}}"#]);
    assert_eq!(
        events,
        [AgentEvent::Unsupported {
            shape: "assistant.video".into()
        }]
    );
}

#[test]
fn a_line_that_is_not_json_is_an_error_naming_what_arrived() {
    let mut stream = ClaudeStream::default();
    let error = stream.push_line("Segmentation fault").unwrap_err();
    assert!(matches!(error, DriverError::Malformed { .. }));
    assert!(error.to_string().contains("Segmentation fault"), "{error}");
}

#[test]
fn a_known_shape_missing_its_required_field_names_the_field() {
    let mut stream = ClaudeStream::default();
    let error = stream
        .push_line(r#"{"type":"assistant","message":{}}"#)
        .unwrap_err();
    assert!(error.to_string().contains("content"), "{error}");
}
