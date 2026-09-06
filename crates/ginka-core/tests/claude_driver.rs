//! The `claude` driver: how a session is launched, resumed, steered and
//! stopped.

use ginka_core::driver::{
    AgentEvent, AgentSession, ClaudeDriver, ProcessSessionSpec as SessionSpec,
    claude::PromptMessage,
};
use ginka_protocol::provider::{AccessMode, OptionOutcome, SessionOptions};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FAKE_AGENT: &str = env!("CARGO_BIN_EXE_ginka-fake-agent");

fn spec() -> SessionSpec {
    SessionSpec {
        binary: PathBuf::from("claude"),
        cwd: PathBuf::from("/repo/worktree"),
        options: SessionOptions {
            model: Some("claude-sonnet-4-5".into()),
            reasoning_effort: None,
            service_tier: None,
            access_mode: AccessMode::Ask,
            account: None,
        },
        resume: None,
    }
}

fn args_of(spec: &SessionSpec) -> Vec<String> {
    ClaudeDriver::command(spec)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_session_is_launched_streaming_in_both_directions() {
    let args = args_of(&spec());
    // Streaming output is what the transcript renders; streaming *input* is
    // what makes steering possible at all.
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--input-format", "stream-json"])
    );
    assert!(args.iter().any(|arg| arg == "--verbose"));
}

#[test]
fn the_session_runs_in_the_worktree_it_belongs_to() {
    let command = ClaudeDriver::command(&spec());
    assert_eq!(
        command.get_current_dir(),
        Some(std::path::Path::new("/repo/worktree"))
    );
}

#[test]
fn the_model_is_passed_only_when_one_was_chosen() {
    let args = args_of(&spec());
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--model", "claude-sonnet-4-5"])
    );

    let mut unset = spec();
    unset.options.model = None;
    assert!(
        !args_of(&unset).iter().any(|arg| arg == "--model"),
        "no model means the CLI's own default, not an empty flag"
    );
}

#[test]
fn each_access_mode_maps_to_a_permission_mode() {
    let mode_for = |access| {
        let mut spec = spec();
        spec.options.access_mode = access;
        let args = args_of(&spec);
        let index = args.iter().position(|arg| arg == "--permission-mode")?;
        args.get(index + 1).cloned()
    };
    // Ours, mapped onto the vendor's vocabulary once, here.
    assert_eq!(mode_for(AccessMode::ReadOnly).as_deref(), Some("plan"));
    // "Edit freely" is the vendor's acceptEdits: its `default` would refuse
    // the edits headless, along with the commands.
    assert_eq!(mode_for(AccessMode::Ask).as_deref(), Some("acceptEdits"));
    assert_eq!(
        mode_for(AccessMode::Auto).as_deref(),
        Some("bypassPermissions")
    );
}

#[test]
fn resuming_names_the_providers_own_session() {
    let mut resumed = spec();
    resumed.resume = Some("sess-1".into());
    let args = args_of(&resumed);
    assert!(args.windows(2).any(|pair| pair == ["--resume", "sess-1"]));

    assert!(
        !args_of(&spec()).iter().any(|arg| arg == "--resume"),
        "a fresh session must not claim to resume one"
    );
}

#[test]
fn a_prompt_is_the_shape_the_cli_reads() {
    let line = PromptMessage::user("review the diff").to_line();
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();

    assert_eq!(parsed["type"], "user");
    assert_eq!(parsed["message"]["role"], "user");
    assert_eq!(parsed["message"]["content"][0]["type"], "text");
    assert_eq!(parsed["message"]["content"][0]["text"], "review the diff");
    assert!(!line.contains('\n'), "one message is one line");
}

#[test]
fn a_prompt_containing_newlines_still_travels_as_one_line() {
    let line = PromptMessage::user("first\nsecond").to_line();
    assert_eq!(line.lines().count(), 1);
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["message"]["content"][0]["text"], "first\nsecond");
}

fn collect_until_exit(events: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut collected = Vec::new();
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                let last = matches!(event, AgentEvent::ProcessExited { .. });
                collected.push(event);
                if last {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    collected
}

/// A session against the fake agent, which echoes whatever is written to it.
fn echoing_session() -> (
    ginka_core::driver::ClaudeSession,
    mpsc::Receiver<AgentEvent>,
) {
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--echo-stdin");
    let session = ClaudeDriver::start_with(command, sender).unwrap();
    (session, receiver)
}

#[test]
fn a_prompt_reaches_the_running_agent() {
    let (mut session, receiver) = echoing_session();
    session.prompt("review the diff").unwrap();
    session.close_input().unwrap();

    let events = collect_until_exit(receiver);
    let echoed = events.iter().any(
        |event| matches!(event, AgentEvent::TextDelta { text } if text.contains("review the diff")),
    );
    assert!(echoed, "{events:?}");
}

#[test]
fn this_transport_can_steer_a_running_turn() {
    let (mut session, receiver) = echoing_session();
    assert!(session.supports_steer());

    session.steer("actually, use serde_json").unwrap();
    session.close_input().unwrap();

    let events = collect_until_exit(receiver);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text } if text.contains("actually, use serde_json")
        )),
        "{events:?}"
    );
}

#[test]
fn a_model_change_rides_on_the_next_turn_rather_than_restarting() {
    let (mut session, _receiver) = echoing_session();
    let outcome = session
        .apply_options(&SessionOptions {
            model: Some("claude-opus-4-5".into()),
            ..spec().options
        })
        .unwrap();
    assert_eq!(outcome, OptionOutcome::Absorbed);
}

#[test]
fn an_access_mode_change_needs_a_new_process() {
    // The driver's own answer, independent of the policy that already refuses
    // to ask: this flag is fixed at launch.
    let (mut session, _receiver) = echoing_session();
    let outcome = session
        .apply_options(&SessionOptions {
            access_mode: AccessMode::Auto,
            ..spec().options
        })
        .unwrap();
    assert_eq!(outcome, OptionOutcome::RestartRequired);
}

#[test]
fn cancelling_a_session_stops_its_process() {
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--hang");
    let mut session = ClaudeDriver::start_with(command, sender).unwrap();

    session.cancel().unwrap();
    let events = collect_until_exit(receiver);
    assert!(
        matches!(events.last(), Some(AgentEvent::ProcessExited { .. })),
        "{events:?}"
    );
}

#[test]
fn the_providers_session_id_is_available_for_the_next_resume() {
    use std::io::Write as _;
    let mut script = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        script,
        r#"{{"type":"system","subtype":"init","session_id":"sess-42"}}"#
    )
    .unwrap();
    script.flush().unwrap();

    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--script").arg(script.path());
    let session = ClaudeDriver::start_with(command, sender).unwrap();
    collect_until_exit(receiver);

    assert_eq!(session.session_id().as_deref(), Some("sess-42"));
}
