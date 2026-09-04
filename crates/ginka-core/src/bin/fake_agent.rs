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
