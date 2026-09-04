//! One event stream, whatever agent produced it.
//!
//! Nothing above the driver boundary knows which vendor is running, so the
//! normalization has to be complete enough that it never has to ask.

use ginka_core::driver::{ActivityItem, ActivityKind};

#[test]
fn a_shell_tool_is_titled_by_the_command_it_runs() {
    let activity = ActivityItem::from_tool(
        Some("call-1".into()),
        "Bash",
        &serde_json::json!({"command": "cargo test --workspace"}),
    );
    assert_eq!(activity.kind, ActivityKind::Command);
    assert_eq!(activity.title, "cargo test --workspace");
    assert_eq!(activity.id.as_deref(), Some("call-1"));
    assert!(!activity.complete, "a call is not its own result");
}

#[test]
fn an_edit_is_titled_by_the_file_it_touches() {
    for (tool, input) in [
        (
            "Edit",
            serde_json::json!({"file_path": "/repo/src/main.rs"}),
        ),
        (
            "Write",
            serde_json::json!({"file_path": "/repo/src/main.rs"}),
        ),
        (
            "Read",
            serde_json::json!({"file_path": "/repo/src/main.rs"}),
        ),
    ] {
        let activity = ActivityItem::from_tool(None, tool, &input);
        assert_eq!(activity.title, "/repo/src/main.rs", "{tool}");
    }
    assert_eq!(
        ActivityItem::from_tool(None, "Edit", &serde_json::json!({"file_path": "x"})).kind,
        ActivityKind::FileChange
    );
    assert_eq!(
        ActivityItem::from_tool(None, "Read", &serde_json::json!({"file_path": "x"})).kind,
        ActivityKind::Tool,
        "reading a file changes nothing"
    );
}

#[test]
fn a_search_is_titled_by_what_was_searched_for() {
    let activity = ActivityItem::from_tool(
        None,
        "Grep",
        &serde_json::json!({"pattern": "AgentEvent", "path": "crates"}),
    );
    assert_eq!(activity.kind, ActivityKind::Search);
    assert_eq!(activity.title, "AgentEvent");
}

#[test]
fn a_plan_tool_is_a_plan() {
    let activity = ActivityItem::from_tool(
        None,
        "TodoWrite",
        &serde_json::json!({"todos": [{"content": "write the parser"}]}),
    );
    assert_eq!(activity.kind, ActivityKind::Plan);
}

#[test]
fn an_explicit_title_beats_everything_the_arguments_could_suggest() {
    let activity = ActivityItem::from_tool(
        None,
        "Bash",
        &serde_json::json!({"command": "rm -rf build", "description": "Clean the build"}),
    );
    assert_eq!(activity.title, "Clean the build");
}

#[test]
fn an_unknown_tool_is_titled_by_its_own_name_made_readable() {
    let activity = ActivityItem::from_tool(None, "WebFetch", &serde_json::json!({}));
    assert_eq!(activity.kind, ActivityKind::Tool);
    assert_eq!(activity.title, "Web fetch");

    let mcp = ActivityItem::from_tool(None, "mcp__linear__create_issue", &serde_json::json!({}));
    assert_eq!(mcp.title, "Create issue");
}

#[test]
fn a_tool_with_nothing_to_say_still_has_a_title() {
    let activity = ActivityItem::from_tool(None, "", &serde_json::json!({}));
    assert!(!activity.title.is_empty());
}

#[test]
fn a_result_completes_the_call_it_belongs_to() {
    let mut activity = ActivityItem::from_tool(
        Some("call-1".into()),
        "Bash",
        &serde_json::json!({"command": "cargo test"}),
    );
    activity.complete_with("test result: ok. 220 passed", false);

    assert!(activity.complete);
    assert!(!activity.failed);
    assert_eq!(
        activity.detail.as_deref(),
        Some("test result: ok. 220 passed")
    );
    assert_eq!(
        activity.title, "cargo test",
        "a result never renames its call"
    );
}

#[test]
fn a_failure_is_marked_and_keeps_its_output() {
    let mut activity =
        ActivityItem::from_tool(None, "Bash", &serde_json::json!({"command": "false"}));
    activity.complete_with("exit code 1", true);
    assert!(activity.failed);
    assert!(activity.complete);
}

#[test]
fn long_output_is_cut_so_one_tool_call_cannot_own_the_transcript() {
    let mut activity =
        ActivityItem::from_tool(None, "Bash", &serde_json::json!({"command": "yes"}));
    activity.complete_with(&"y\n".repeat(100_000), false);

    let detail = activity.detail.unwrap();
    assert!(
        detail.len() <= ActivityItem::MAX_DETAIL_BYTES + 64,
        "{}",
        detail.len()
    );
    assert!(detail.ends_with("… truncated\n") || detail.ends_with("… truncated"));
}

#[test]
fn a_multibyte_output_is_cut_on_a_character_boundary() {
    let mut activity =
        ActivityItem::from_tool(None, "Bash", &serde_json::json!({"command": "cat"}));
    activity.complete_with(&"あ".repeat(100_000), false);
    // Reaching here means the cut landed on a boundary rather than panicking.
    assert!(activity.detail.unwrap().contains('あ'));
}

#[test]
fn a_title_is_a_single_line_however_the_arguments_arrived() {
    let activity = ActivityItem::from_tool(
        None,
        "Bash",
        &serde_json::json!({"command": "cargo test \\\n  --workspace"}),
    );
    assert!(!activity.title.contains('\n'), "{}", activity.title);
}
