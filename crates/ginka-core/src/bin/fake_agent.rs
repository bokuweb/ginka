//! A scripted stand-in for a vendor's agent CLI.
//!
//! Session behaviour — supervision, cancellation, queued follow-ups, resume —
//! has to be testable without a network, an account or a token budget, so the
//! tests run this instead of `claude` (`docs/roadmap.md` §6.1). It speaks
//! Claude Code's `stream-json`, which means the real driver is what parses it.
//!
//! The script is a file named by `GINKA_FAKE_AGENT_SCRIPT`, one directive per
//! line:
//!
//! - `#sleep <ms>` — wait, so a test can cancel mid-turn.
//! - `#read` — block until a line arrives on stdin.
//! - `#stderr <text>` — write to stderr.
//! - `#spawn <file>` — start a long-lived child and write its pid to `file`,
//!   so a test can check that cancelling reached the whole process tree and
//!   not only the agent.
//! - `#exit <code>` — exit immediately with that status.
//! - anything else — write the line to stdout verbatim.
//!
//! `{prompt}` in a line is replaced with the last command-line argument, which
//! is where every driver in this workspace puts the prompt. `{session}` is
//! replaced with `GINKA_FAKE_AGENT_SESSION`, defaulting to `fake-session`, and
//! `{args}` with the whole command line — which is how a test asserts that a
//! follow-up was started as a resume rather than as a fresh session.

use std::io::{BufRead, Write};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let prompt = arguments.last().cloned().unwrap_or_default();
    let joined = arguments.join(" ");
    let session =
        std::env::var("GINKA_FAKE_AGENT_SESSION").unwrap_or_else(|_| "fake-session".to_string());

    let script = match std::env::var("GINKA_FAKE_AGENT_SCRIPT") {
        Ok(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading the script at {path}: {error}")),
        // With no script, behave like a well-behaved agent that answers once.
        Err(_) => default_script(),
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut stdin = std::io::stdin().lock();

    for line in script.lines() {
        let line = line
            .replace("{prompt}", &prompt)
            .replace("{session}", &session)
            .replace("{args}", &joined);
        if let Some(rest) = line.strip_prefix("#sleep ") {
            let millis: u64 = rest.trim().parse().unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(millis));
        } else if line.trim() == "#read" {
            let mut buffer = String::new();
            // End of input means the parent gave up on us; stop rather than
            // spinning on a closed pipe.
            if stdin.read_line(&mut buffer).unwrap_or(0) == 0 {
                return;
            }
        } else if let Some(rest) = line.strip_prefix("#spawn ") {
            // A stand-in for the compiler or test runner a real agent starts.
            // Deliberately not waited on: the point is that it outlives this
            // process unless something kills the whole group.
            #[allow(clippy::zombie_processes)]
            let child = std::process::Command::new("sleep")
                .arg("300")
                .spawn()
                .expect("sleep is on PATH");
            std::fs::write(rest.trim(), child.id().to_string()).expect("the pid file is writable");
        } else if let Some(rest) = line.strip_prefix("#stderr ") {
            eprintln!("{rest}");
        } else if let Some(rest) = line.strip_prefix("#exit ") {
            let code: i32 = rest.trim().parse().unwrap_or(0);
            out.flush().ok();
            std::process::exit(code);
        } else {
            writeln!(out, "{line}").expect("stdout is open");
            // Flushed per line: a test that waits for an event must not be
            // held up by a buffer that only empties at exit.
            out.flush().expect("stdout is open");
        }
    }
}

/// One system line, one answer, one result — the shape of a short session.
fn default_script() -> String {
    [
        r#"{"type":"system","subtype":"init","session_id":"{session}","model":"fake"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"answering: {prompt}"}]},"session_id":"{session}"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"answering: {prompt}","session_id":"{session}","usage":{"input_tokens":1,"output_tokens":1}}"#,
    ]
    .join("\n")
}
