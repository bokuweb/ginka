//! A stand-in for a vendor's CLI, for tests that need a real process.
//!
//! Session behaviour is pinned against this rather than a live agent: no
//! tokens, no network, and the awkward cases — a process that hangs after its
//! last line, one that exits non-zero, one that prints a warning on stdout —
//! are scripted here instead of waited for in the wild.
//!
//! It is a normal binary of this crate so integration tests can find it
//! through `CARGO_BIN_EXE_ginka-fake-agent`.

use std::io::{BufRead, Write};

fn main() {
    // Two ways to script this, because two suites do. A script file exercises
    // the daemon's supervisor with directives it can pause and block on; the
    // flags exercise the driver's own reading of a stream. Neither knows about
    // the other, so the file wins when it is set.
    if std::env::var_os("GINKA_FAKE_AGENT_SCRIPT").is_some() {
        return scripted();
    }
    flags();
}

/// Flag-driven: what the driver tests script.
fn flags() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut exit_code = 0;
    let mut hang = false;
    let mut echo_stdin = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            // Print each line of a file, in order, as the agent's output.
            "--script" => {
                index += 1;
                let path = args.get(index).expect("--script needs a path");
                let text = std::fs::read_to_string(path).expect("script is readable");
                for line in text.lines() {
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                }
            }
            // Answer each line on stdin with an assistant message carrying it.
            "--echo-stdin" => echo_stdin = true,
            // Stay alive with nothing to say, so a test can cancel it.
            "--hang" => hang = true,
            "--exit-code" => {
                index += 1;
                exit_code = args[index].parse().expect("--exit-code needs a number");
            }
            // Something that is not a message at all, on stdout.
            "--noise" => println!("warning: this line is not JSON"),
            // Stand in for a CLI answering a version probe.
            "--version" => {
                println!("ginka-fake-agent 9.9.9 (Claude Code compatible)");
                std::process::exit(0);
            }
            // A CLI that answers a probe with nothing useful.
            "--silent-version" => {
                println!("hello");
                std::process::exit(0);
            }
            other => panic!("unknown argument {other}"),
        }
        index += 1;
    }

    if echo_stdin {
        for line in std::io::stdin().lock().lines() {
            let line = line.expect("stdin is readable");
            let payload = serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": line}]}
            });
            println!("{payload}");
            let _ = std::io::stdout().flush();
        }
    }

    if hang {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3_600));
        }
    }
    std::process::exit(exit_code);
}

/// File-driven: what the daemon's session tests script.
fn scripted() {
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
