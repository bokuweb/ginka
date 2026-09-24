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
    if std::env::args().any(|argument| argument == "--lsp") {
        return language_server();
    }
    if std::env::args().any(|argument| argument == "--experimental-acp" || argument == "acp") {
        return acp_agent();
    }
    // Two ways to script this, because two suites do. A script file exercises
    // the daemon's supervisor with directives it can pause and block on; the
    // flags exercise the driver's own reading of a stream. Neither knows about
    // the other, so the file wins when it is set.
    if std::env::var_os("GINKA_FAKE_AGENT_SCRIPT").is_some() {
        return scripted();
    }
    flags();
}

/// Minimal ACP agent: one session, reloadable, that echoes the prompt back.
/// A prompt that mentions "permission" asks for one first and says which
/// option came back.
fn acp_agent() {
    use serde_json::{Value, json};
    let mut output = std::io::stdout().lock();
    let mut send = |message: Value| {
        writeln!(output, "{message}").ok();
        output.flush().ok();
    };
    let chunk = |session: &str, text: &str| {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session,
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}})
    };
    let mut lines = std::io::stdin().lock().lines();
    let mut pending_prompt: Option<(Value, String, String)> = None;
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("initialize") => send(json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": 1, "agentCapabilities": {"loadSession": true}}})),
            Some("session/new") => {
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"sessionId": "acp-1"}}))
            }
            Some("session/load") => {
                let session = message["params"]["sessionId"].as_str().unwrap_or_default();
                send(chunk(session, "[replayed]"));
                send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
            }
            Some("session/prompt") => {
                let session = message["params"]["sessionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let text = message["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if text.contains("permission") {
                    send(
                        json!({"jsonrpc": "2.0", "id": "ask-1", "method": "session/request_permission",
                        "params": {"sessionId": session, "toolCall": {"toolCallId": "c1", "title": "Touch a file"},
                        "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}}),
                    );
                    pending_prompt = Some((id, session, text));
                    continue;
                }
                send(chunk(&session, &format!("[acp:{session}:{text}]")));
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}));
            }
            None => {
                if let Some((prompt, session, _)) = pending_prompt.take() {
                    let chose = message["result"]["outcome"]["optionId"]
                        .as_str()
                        .unwrap_or("cancelled");
                    send(chunk(&session, &format!("[permission:{chose}]")));
                    send(
                        json!({"jsonrpc": "2.0", "id": prompt, "result": {"stopReason": "end_turn"}}),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Minimal LSP peer for exercising framing and document synchronization.
fn language_server() {
    let mut input = std::io::BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    let mut document_uri = serde_json::Value::Null;
    let mut document_text = String::new();

    while let Some(message) = read_lsp_message(&mut input) {
        let method = message
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match method {
            "initialize" => write_lsp_message(
                &mut output,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "capabilities": {
                            "textDocumentSync": 1,
                            "hoverProvider": true,
                            "definitionProvider": true
                        }
                    }
                }),
            ),
            "textDocument/didOpen" => {
                document_uri = message["params"]["textDocument"]["uri"].clone();
                document_text = message["params"]["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                write_lsp_message(
                    &mut output,
                    &serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {
                            "uri": document_uri,
                            "diagnostics": [{
                                "range": {
                                    "start": {"line": 0, "character": 0},
                                    "end": {"line": 0, "character": 2}
                                },
                                "message": "fake diagnostic"
                            }]
                        }
                    }),
                );
            }
            "textDocument/didChange" => {
                document_text = message["params"]["contentChanges"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
            }
            "textDocument/hover" => write_lsp_message(
                &mut output,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {"contents": document_text}
                }),
            ),
            "textDocument/definition" => write_lsp_message(
                &mut output,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": [{
                        "targetUri": document_uri,
                        "targetRange": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 2}
                        },
                        "targetSelectionRange": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 2}
                        }
                    }]
                }),
            ),
            "exit" => return,
            _ => {}
        }
    }
}

fn read_lsp_message(reader: &mut impl BufRead) -> Option<serde_json::Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; content_length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_lsp_message(writer: &mut impl Write, message: &serde_json::Value) {
    let body = serde_json::to_vec(message).expect("LSP response serializes");
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).expect("stdout is open");
    writer.write_all(&body).expect("stdout is open");
    writer.flush().expect("stdout is open");
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
    // A driver that streams its input passes no prompt on the command line;
    // the first thing written to stdin is the prompt instead. The flag is how
    // to tell, because the last argument is a flag's value either way.
    let streamed_input = arguments.iter().any(|arg| arg == "--input-format");
    let prompt = if streamed_input {
        String::new()
    } else {
        arguments.last().cloned().unwrap_or_default()
    };
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
    // The last line read by `#read`, so a script can say what it was given.
    let mut last_read = String::new();
    // So a script can keep saying `{prompt}` whichever way it arrived.
    let mut prompt = prompt;
    if prompt.is_empty() {
        let mut first = String::new();
        if stdin.read_line(&mut first).unwrap_or(0) > 0 {
            prompt = user_text(&first);
        }
    }

    for line in script.lines() {
        // The `_json` forms are the same values as JSON strings, quotes
        // included, for a script that has to carry a prompt with newlines in
        // it through a JSONL line intact.
        let line = line
            .replace("{prompt_json}", &json_string(&prompt))
            .replace("{args_json}", &json_string(&joined))
            .replace("{prompt}", &prompt)
            .replace("{session}", &session)
            .replace("{stdin}", &last_read)
            .replace("{args}", &joined);
        // `{env:NAME}` says what the agent was given, which is how a test
        // sees that an account's directory reached it.
        let line = substitute_env(&line);
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
            last_read = user_text(&buffer);
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

/// `text` as a JSON string literal, quotes and escapes included.
fn json_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string always serializes")
}

/// Replace every `{env:NAME}` with the variable's value, or with nothing
/// when it is not set: an unset variable is a fact a test may want to see.
fn substitute_env(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("{env:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{env:".len()..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push_str(&std::env::var(&after[..end]).unwrap_or_default());
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
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

/// What a user message says.
///
/// A streamed input is a JSON message; a script wants the sentence inside it,
/// which is what a real agent acts on. Anything that is not one of those is
/// taken as it came.
fn user_text(line: &str) -> String {
    let line = line.trim();
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("message")?
                .get("content")?
                .get(0)?
                .get("text")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| line.to_string())
}
