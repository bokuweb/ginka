//! Claude Code's stream-json, normalized into the one event stream.
//!
//! Recorded shapes rather than a live CLI: vendors change these without
//! notice, and a fixture is the only thing that tells us *which* line stopped
//! being understood (roadmap R6).

use ginka_core::driver::{ActivityKind, AgentEvent, ClaudeStream, DriverError};
use ginka_protocol::model::SessionState;
use ginka_protocol::{SubagentStep, SubagentStepKind, SubagentStepStatus, TaskItem, TaskStatus};

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
fn a_todo_write_exposes_the_same_task_shape_as_other_drivers() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tasks-1","name":"TodoWrite","input":{"todos":[{"content":"Inspect","status":"completed"},{"content":"Implement","status":"in_progress"}]}}]}}"#,
    ]);

    let AgentEvent::ToolCall { activity } = &events[0] else {
        panic!("TodoWrite must remain a normalized tool call")
    };
    assert_eq!(activity.kind, ActivityKind::Plan);
    assert_eq!(
        activity.tasks,
        Some(vec![
            TaskItem::new("Inspect", TaskStatus::Completed),
            TaskItem::new("Implement", TaskStatus::InProgress),
        ])
    );
}

#[test]
fn a_subagents_work_stays_under_the_agent_call_that_spawned_it() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"agent-1","name":"Agent","input":{"description":"Review correctness"}}]}}"#,
        r#"{"type":"assistant","parent_tool_use_id":"agent-1","message":{"id":"message-1","content":[{"type":"thinking","thinking":"Start with the reducer."},{"type":"text","text":"I will inspect the state transitions."},{"type":"tool_use","id":"read-1","name":"Read","input":{"file_path":"src/state.rs"}}]}}"#,
        r#"{"type":"user","parent_tool_use_id":"agent-1","message":{"content":[{"type":"tool_result","tool_use_id":"read-1","content":"done","is_error":false}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"agent-1","content":"No regressions found.","is_error":false}]}}"#,
    ]);

    assert_eq!(
        events,
        [
            AgentEvent::SubagentStarted {
                id: "agent-1".into(),
                title: "Review correctness".into(),
            },
            AgentEvent::SubagentStep {
                parent_id: "agent-1".into(),
                step: SubagentStep::new(
                    "message-1:thinking",
                    SubagentStepKind::Reasoning,
                    "Start with the reducer.",
                ),
            },
            AgentEvent::SubagentStep {
                parent_id: "agent-1".into(),
                step: SubagentStep::new(
                    "message-1:text",
                    SubagentStepKind::Message,
                    "I will inspect the state transitions.",
                ),
            },
            AgentEvent::SubagentStep {
                parent_id: "agent-1".into(),
                step: SubagentStep::new("read-1", SubagentStepKind::Tool, "src/state.rs")
                    .with_status(SubagentStepStatus::Running),
            },
            AgentEvent::SubagentStep {
                parent_id: "agent-1".into(),
                step: SubagentStep::new("read-1", SubagentStepKind::Tool, "")
                    .with_status(SubagentStepStatus::Completed),
            },
            AgentEvent::SubagentFinished {
                id: "agent-1".into(),
                summary: Some("No regressions found.".into()),
                failed: false,
            },
        ]
    );
}

#[test]
fn an_orphan_child_message_is_not_promoted_into_the_parent_transcript() {
    let events = events(&[
        r#"{"type":"assistant","parent_tool_use_id":"missing-agent","message":{"id":"message-1","content":[{"type":"text","text":"private child output"}]}}"#,
    ]);

    assert!(events.is_empty());
}

#[test]
fn the_turn_result_settles_a_subagent_whose_report_never_arrived() {
    let events = events(&[
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"agent-1","name":"Agent","input":{"description":"Review correctness"}}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Done."}"#,
    ]);

    assert_eq!(
        events[1],
        AgentEvent::SubagentFinished {
            id: "agent-1".into(),
            summary: None,
            failed: false,
        }
    );
    assert!(matches!(events[2], AgentEvent::TurnEnd { turn: 1 }));
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
fn a_screenshot_result_keeps_its_complete_data_url_for_daemon_externalization() {
    let mut stream = ClaudeStream::default();
    stream
        .push_line(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"shot","name":"Screenshot","input":{}}]}}"#,
        )
        .unwrap();
    let data = "A".repeat(20_000);
    let result = serde_json::json!({
        "type": "user",
        "message": {"content": [{
            "type": "tool_result",
            "tool_use_id": "shot",
            "content": [{
                "type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": data}
            }]
        }]}
    })
    .to_string();

    let events = stream.push_line(&result).unwrap();
    let AgentEvent::ToolResult { activity } = &events[0] else {
        panic!("expected the screenshot result")
    };
    let detail = activity.detail.as_deref().unwrap();
    assert!(detail.starts_with("data:image/png;base64,"));
    assert!(detail.len() > ginka_protocol::event::ActivityItem::MAX_DETAIL_BYTES);
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

// ---------------------------------------------------------------------------
// A refused turn is the one thing the CLI says about its rate-limit windows
// headless (`docs/accounts.md` §6).

fn plan_events(events: &[AgentEvent]) -> Vec<ginka_protocol::model::PlanUsage> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::PlanUsage { usage } => Some(usage.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_refused_turn_is_a_window_at_the_wall_with_the_reset_it_named() {
    let mut stream = ClaudeStream::default();
    let events = stream
        .push_line(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Claude AI usage limit reached|1700000000","session_id":"s"}"#)
        .unwrap();
    let plans = plan_events(&events);
    assert_eq!(plans.len(), 1);
    let window = &plans[0].windows[0];
    assert_eq!(
        window.label, "limit",
        "the older wording does not say which"
    );
    assert_eq!(window.used_percent, 100.0);
    assert_eq!(window.resets_at, Some(1_700_000_000));
    // And the turn still failed, as it did.
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SessionResult {
            state: SessionState::Failed,
            ..
        }
    )));
}

#[test]
fn the_newer_wording_names_the_window_and_gives_no_number_to_reset_by() {
    let mut stream = ClaudeStream::default();
    let events = stream
        .push_line(r#"{"type":"result","subtype":"error","is_error":true,"result":"5-hour limit reached ∙ resets 3pm","session_id":"s"}"#)
        .unwrap();
    let plans = plan_events(&events);
    assert_eq!(plans[0].windows[0].label, "5h");
    assert_eq!(
        plans[0].windows[0].resets_at, None,
        "3pm is prose, and a guessed timestamp would be worse than none"
    );

    let mut stream = ClaudeStream::default();
    let events = stream
        .push_line(r#"{"type":"result","subtype":"error","is_error":true,"result":"Opus weekly limit reached","session_id":"s"}"#)
        .unwrap();
    assert_eq!(plan_events(&events)[0].windows[0].label, "opus week");
}

#[test]
fn one_wall_said_twice_in_a_turn_is_one_reading() {
    // The CLI puts the refusal in the assistant's text and in the result.
    let mut stream = ClaudeStream::default();
    let mut events = stream
        .push_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Claude AI usage limit reached|1700000000"}]},"session_id":"s"}"#)
        .unwrap();
    events.extend(
        stream
            .push_line(r#"{"type":"result","subtype":"error","is_error":true,"result":"Claude AI usage limit reached|1700000000","session_id":"s"}"#)
            .unwrap(),
    );
    assert_eq!(plan_events(&events).len(), 1);
}

#[test]
fn an_ordinary_failure_claims_nothing_about_the_windows() {
    let mut stream = ClaudeStream::default();
    let events = stream
        .push_line(r#"{"type":"result","subtype":"error","is_error":true,"result":"Invalid API key · Please run /login","session_id":"s"}"#)
        .unwrap();
    assert!(plan_events(&events).is_empty());
    // Nor does an answer that happens to mention limits.
    let mut stream = ClaudeStream::default();
    let events = stream
        .push_line(r#"{"type":"result","subtype":"success","is_error":false,"result":"Set a limit reached by the loop counter.","session_id":"s"}"#)
        .unwrap();
    assert!(
        plan_events(&events).is_empty(),
        "a successful turn hit no wall"
    );
}
