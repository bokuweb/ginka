//! Supervising a real agent process: what reaches the transcript, and what
//! happens when it is stopped.

use ginka_core::driver::{AgentEvent, AgentProcess, TurnOutcome};
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FAKE_AGENT: &str = env!("CARGO_BIN_EXE_ginka-fake-agent");

fn script(lines: &[&str]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    file.flush().unwrap();
    file
}

/// Drain until the process is gone, so a test never depends on timing.
fn collect(events: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
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

#[test]
fn a_session_streams_its_output_and_then_reports_its_exit() {
    let script = script(&[
        r#"{"type":"system","subtype":"init","session_id":"s1","model":"m"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#,
    ]);
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--script").arg(script.path());

    let process = AgentProcess::spawn(command, sender).unwrap();
    let events = collect(receiver);

    assert!(matches!(events[0], AgentEvent::Connected { .. }));
    assert!(events.contains(&AgentEvent::TextDelta("hello".into())));
    assert!(events.contains(&AgentEvent::TurnEnd {
        outcome: TurnOutcome::Completed
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::ProcessExited { code: Some(0) })
    );
    assert!(!process.is_running());
}

#[test]
fn the_session_id_is_available_for_a_resume() {
    let script = script(&[r#"{"type":"system","subtype":"init","session_id":"s1"}"#]);
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--script").arg(script.path());

    let process = AgentProcess::spawn(command, sender).unwrap();
    collect(receiver);
    assert_eq!(process.session_id().as_deref(), Some("s1"));
}

#[test]
fn a_non_zero_exit_is_reported_rather_than_swallowed() {
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--exit-code").arg("3");

    // The handle has to outlive the reading: dropping it cancels the agent.
    let _process = AgentProcess::spawn(command, sender).unwrap();
    let events = collect(receiver);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::ProcessExited { code: Some(3) })
    );
}

#[test]
fn a_line_that_is_not_a_message_does_not_derail_the_session() {
    // Agents print warnings on stdout. One is not a reason to end a turn.
    let script =
        script(&[r#"{"type":"assistant","message":{"content":[{"type":"text","text":"after"}]}}"#]);
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--noise").arg("--script").arg(script.path());

    // The handle has to outlive the reading: dropping it cancels the agent.
    let _process = AgentProcess::spawn(command, sender).unwrap();
    let events = collect(receiver);
    assert!(events.contains(&AgentEvent::TextDelta("after".into())));
}

#[test]
fn a_contract_that_moved_is_surfaced_in_the_transcript() {
    // A shape we recognise, in a form we do not: the user has to be told, or
    // the session simply goes quiet with no explanation.
    let script = script(&[r#"{"type":"assistant","message":{}}"#]);
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--script").arg(script.path());

    // The handle has to outlive the reading: dropping it cancels the agent.
    let _process = AgentProcess::spawn(command, sender).unwrap();
    let events = collect(receiver);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Unsupported { .. })),
        "{events:?}"
    );
}

#[test]
fn what_is_written_to_the_agent_reaches_it() {
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--echo-stdin");

    let mut process = AgentProcess::spawn(command, sender).unwrap();
    process.send_line(r#"{"prompt":"do the thing"}"#).unwrap();
    process.close_input().unwrap();

    let events = collect(receiver);
    assert!(
        events.contains(&AgentEvent::TextDelta(
            r#"{"prompt":"do the thing"}"#.into()
        )),
        "{events:?}"
    );
}

#[test]
fn cancelling_ends_a_process_that_would_otherwise_never_stop() {
    let (sender, receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--hang");

    let mut process = AgentProcess::spawn(command, sender).unwrap();
    assert!(process.is_running());

    let started = Instant::now();
    process.cancel().unwrap();
    let events = collect(receiver);

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancel has to be prompt"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::ProcessExited { .. })),
        "{events:?}"
    );
    assert!(!process.is_running());
}

#[test]
fn cancelling_twice_is_not_an_error() {
    let (sender, _receiver) = mpsc::channel();
    let mut command = std::process::Command::new(FAKE_AGENT);
    command.arg("--hang");

    let mut process = AgentProcess::spawn(command, sender).unwrap();
    process.cancel().unwrap();
    process.cancel().unwrap();
}

#[test]
fn a_binary_that_does_not_exist_fails_at_the_spawn_with_its_name() {
    let (sender, _receiver) = mpsc::channel();
    let command = std::process::Command::new("ginka-no-such-agent-binary");
    let error = AgentProcess::spawn(command, sender)
        .unwrap_err()
        .to_string();
    assert!(error.contains("ginka-no-such-agent-binary"), "{error}");
}
