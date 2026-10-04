//! Supervising an agent's process.
//!
//! One child process, one thread reading its stdout, and a channel carrying
//! normalized events out. The reader is a thread rather than an async task on
//! purpose: it spends its life blocked on a pipe, which is what threads are
//! for, and it keeps the driver traits synchronous (`docs/roadmap.md` §4.5).
//!
//! Two behaviours are load-bearing and both are tested against a real child:
//! a line that is not a message must not end a turn — agents print warnings on
//! stdout — while a message we recognise in a shape we do not must reach the
//! transcript, or the session simply goes quiet with no explanation (R6).

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::driver::{AgentEvent, ClaudeStream, DriverError};

/// A running agent CLI.
#[derive(Debug)]
pub struct AgentProcess {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    finished: Arc<AtomicBool>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl AgentProcess {
    /// Spawn the agent and start reading it.
    ///
    /// The caller builds the `Command` — arguments and environment are the
    /// driver's business, not this module's — and this adds the pipes.
    pub fn spawn(mut command: Command, events: Sender<AgentEvent>) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The agent's own diagnostics are not transcript content; they go
            // to the daemon's log, not into the user's conversation.
            .stderr(Stdio::null());

        // A new process group, so cancelling can take the agent's own children
        // with it: an agent that spawned a build is not stopped by killing the
        // agent alone.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }

        let program = command.get_program().to_string_lossy().into_owned();
        let mut child = command
            .spawn()
            .with_context(|| format!("starting the agent binary {program}"))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("agent stdout was not piped")?;
        let child = Arc::new(Mutex::new(child));
        let finished = Arc::new(AtomicBool::new(false));
        let session_id = Arc::new(Mutex::new(None));

        let reader = std::thread::Builder::new()
            .name(format!("agent-reader:{program}"))
            .spawn({
                let child = Arc::clone(&child);
                let finished = Arc::clone(&finished);
                let session_id = Arc::clone(&session_id);
                move || read_stream(stdout, events, child, finished, session_id)
            })
            .context("starting the agent reader thread")?;

        Ok(Self {
            child,
            stdin,
            reader: Some(reader),
            finished,
            session_id,
        })
    }

    /// The provider's session id, once it has introduced itself. This is what
    /// a resume is built from.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|id| id.clone())
    }

    /// True until the reader thread has seen stdout close and reaped the child.
    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
    }

    /// Write one line to the agent's stdin.
    pub fn send_line(&mut self, line: &str) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("the agent's input has already been closed")?;
        writeln!(stdin, "{line}").context("writing to the agent")?;
        stdin.flush().context("flushing to the agent")?;
        Ok(())
    }

    /// Close stdin. For the transports whose "no more input" *is* end-of-file,
    /// this is how a turn is ended without a signal.
    pub fn close_input(&mut self) -> Result<()> {
        self.stdin.take();
        Ok(())
    }

    /// Stop the agent, and anything it started.
    ///
    /// Cancelling an already-finished process is not an error: a turn that
    /// ended on its own while the user was reaching for stop is the ordinary
    /// race, not a failure.
    pub fn cancel(&mut self) -> Result<()> {
        self.stdin.take();
        if !self.is_running() {
            return Ok(());
        }

        let pid = {
            let mut child = self.child.lock().expect("agent process lock");
            let pid = child.id();
            // Killing the child alone leaves whatever it spawned running.
            #[cfg(unix)]
            terminate_group(pid);
            let _ = child.kill();
            pid
        };
        tracing::debug!(pid, "cancelled an agent process");
        Ok(())
    }

    /// Wait for the reader to drain and the process to be reaped.
    pub fn wait(&mut self) -> Result<()> {
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        // A dropped handle must not leave an agent running: the daemon holds
        // these per session, and a leaked process keeps burning tokens.
        let _ = self.cancel();
    }
}

/// Signal the whole process group when the child demonstrably leads one.
///
/// `kill(1)` is used rather than `libc::kill` because the workspace denies
/// `unsafe_code`, and a subprocess at cancel time costs nothing next to the
/// process it is ending. The group checks matter even though spawn requests a
/// new group: signalling an unverified negative PID can terminate the daemon's
/// own host when a platform or launch race did not honour that request.
#[cfg(unix)]
fn terminate_group(pid: u32) {
    let Some(target) = group_signal_target(
        pid,
        process_group_of(std::process::id()),
        process_group_of(pid),
    ) else {
        tracing::warn!(pid, "agent does not lead a separate process group");
        return;
    };
    let _ = Command::new("kill")
        .args(["-TERM", "--", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
fn process_group_of(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn group_signal_target(
    pid: u32,
    current_group: Option<u32>,
    child_group: Option<u32>,
) -> Option<String> {
    (pid != 0 && child_group == Some(pid) && child_group != current_group)
        .then(|| format!("-{pid}"))
}

fn read_stream(
    stdout: std::process::ChildStdout,
    events: Sender<AgentEvent>,
    child: Arc<Mutex<Child>>,
    finished: Arc<AtomicBool>,
    session_id: Arc<Mutex<Option<String>>>,
) {
    let mut stream = ClaudeStream::default();
    let reader = BufReader::new(stdout);

    for line in reader.lines() {
        let Ok(line) = line else { break };
        match stream.push_line(&line) {
            Ok(parsed) => {
                if let Ok(mut held) = session_id.lock()
                    && held.is_none()
                {
                    *held = stream.session_id().map(str::to_string);
                }
                for event in parsed {
                    if events.send(event).is_err() {
                        // Nobody is listening any more; the session outlived
                        // its consumer and there is nothing to supervise for.
                        return;
                    }
                }
            }
            Err(DriverError::Malformed { line }) => {
                // Warnings, banners, a stray panic message: logged, not shown.
                tracing::warn!(line, "agent wrote a line that is not a message");
            }
            Err(error @ DriverError::MissingField { .. }) => {
                tracing::error!(%error, "the agent's output no longer matches this build");
                let _ = events.send(AgentEvent::Unsupported {
                    shape: error.to_string(),
                });
            }
        }
    }

    let code = child
        .lock()
        .ok()
        .and_then(|mut child| child.wait().ok())
        .and_then(|status| status.code());
    finished.store(true, Ordering::Release);
    let _ = events.send(AgentEvent::ProcessExited { code });
}

#[cfg(test)]
mod tests {
    use super::group_signal_target;

    #[test]
    fn only_a_confirmed_foreign_process_group_can_be_signalled() {
        assert_eq!(
            group_signal_target(42, Some(7), Some(42)),
            Some("-42".into())
        );
        assert_eq!(group_signal_target(42, Some(42), Some(42)), None);
        assert_eq!(group_signal_target(42, Some(7), Some(7)), None);
        assert_eq!(group_signal_target(42, Some(7), None), None);
    }
}
