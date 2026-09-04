//! The shells the daemon runs, one per terminal.
//!
//! The daemon owns the pty rather than the window, which is the whole point of
//! the process split: a build started in a terminal keeps running when the
//! window closes, and is still there when it opens again.
//!
//! What comes out is forwarded as bytes, escapes and all. Deciding what they
//! mean belongs to the client, because the client is the one with a screen —
//! and a daemon that interpreted them would have to guess at a size it cannot
//! see.

use crate::service::EventSink;
use anyhow::{Context, Result};
use ginka_protocol::{DaemonEvent, TerminalId};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// How much output is read at once.
///
/// A build's output arrives in bursts; a bigger buffer means fewer wake-ups
/// per burst, and 8 KiB is about a screen of dense text.
const READ_CHUNK: usize = 8 * 1024;

/// One running shell.
struct Running {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Dropping this kills the shell.
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Every terminal the daemon is running.
pub struct Terminals {
    running: Mutex<HashMap<TerminalId, Running>>,
    events: Arc<dyn EventSink>,
}

impl Terminals {
    /// Build a pool that announces its output through `events`.
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            events,
        }
    }

    /// Start a shell in `worktree`.
    ///
    /// The user's own shell, from `SHELL`, because a terminal that is not the
    /// one they configured is a terminal that behaves unlike every other one
    /// they use.
    pub fn open(&self, worktree: &Path, rows: u16, cols: u16) -> Result<TerminalId> {
        let system = NativePtySystem::default();
        let pair = system
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening a pty")?;

        let mut command = CommandBuilder::new(shell());
        command.cwd(worktree);
        // Announced as a terminal that understands colour, because that is
        // what the client renders.
        command.env("TERM", "xterm-256color");
        for (name, _) in std::env::vars() {
            if crate::agent::is_inherited_session_state(&name) {
                command.env_remove(&name);
            }
        }

        let child = pair
            .slave
            .spawn_command(command)
            .context("starting a shell")?;
        // The slave is the shell's end; holding it open would keep the pty
        // alive after the shell exits, and the reader would never see EOF.
        drop(pair.slave);

        let writer = pair.master.take_writer().context("taking the pty writer")?;
        let reader = pair.master.try_clone_reader().context("reading the pty")?;
        let id = TerminalId(uuid::Uuid::new_v4().simple().to_string());

        self.pump(id.clone(), reader);
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                Running {
                    writer,
                    master: pair.master,
                    child,
                },
            );
        Ok(id)
    }

    /// Forward everything the shell prints, until it stops.
    ///
    /// On its own thread rather than the async executor: a pty read is a
    /// blocking file read with no async form on every platform, and one shell
    /// producing output must not occupy a task slot the agents are using.
    fn pump(&self, id: TerminalId, mut reader: Box<dyn std::io::Read + Send>) {
        let events = self.events.clone();
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        // Lossy on purpose: a shell can emit a partial UTF-8
                        // sequence across two reads, and refusing to forward
                        // the chunk would stall the screen over one byte.
                        let data = String::from_utf8_lossy(&buffer[..read]).to_string();
                        events.emit(DaemonEvent::TerminalOutput {
                            terminal: id.clone(),
                            data,
                        });
                    }
                }
            }
            events.emit(DaemonEvent::TerminalClosed { terminal: id });
        });
    }

    /// Send keystrokes to a terminal.
    pub fn write(&self, id: &TerminalId, data: &str) -> Result<()> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let terminal = running
            .get_mut(id)
            .with_context(|| format!("no terminal with id {id}"))?;
        terminal.writer.write_all(data.as_bytes())?;
        terminal.writer.flush()?;
        Ok(())
    }

    /// Tell a terminal how big its window is now.
    ///
    /// Without this a program that draws a full-screen interface — an editor,
    /// a pager, an agent's own progress display — lays out for the wrong size.
    pub fn resize(&self, id: &TerminalId, rows: u16, cols: u16) -> Result<()> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let terminal = running
            .get(id)
            .with_context(|| format!("no terminal with id {id}"))?;
        terminal.master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Stop a terminal's shell and forget it.
    pub fn close(&self, id: &TerminalId) -> Result<()> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut terminal) = running.remove(id) {
            terminal.child.kill().ok();
            terminal.child.wait().ok();
        }
        Ok(())
    }

    /// How many shells are running. For the daemon's own logging, and tests.
    pub fn count(&self) -> usize {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// The shell to run.
///
/// The user's own, because a terminal that is not the one they configured is a
/// terminal that behaves unlike every other one they use. `/bin/sh` only when
/// the environment says nothing, which is a machine with no login shell set.
fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::DaemonEvent;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<DaemonEvent>>,
    }

    impl EventSink for Recorder {
        fn emit(&self, event: DaemonEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    impl Recorder {
        /// Everything a terminal has printed so far.
        fn printed(&self, id: &TerminalId) -> String {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|event| match event {
                    DaemonEvent::TerminalOutput { terminal, data } if terminal == id => {
                        Some(data.clone())
                    }
                    _ => None,
                })
                .collect()
        }

        fn closed(&self, id: &TerminalId) -> bool {
            self.events.lock().unwrap().iter().any(
                |event| matches!(event, DaemonEvent::TerminalClosed { terminal } if terminal == id),
            )
        }
    }

    /// Wait for `check` to hold, or give up.
    fn until(check: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_terminal_runs_a_shell_and_forwards_what_it_prints() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let terminals = Terminals::new(recorder.clone());

        let id = terminals.open(dir.path(), 24, 80).unwrap();
        terminals.write(&id, "echo ginka-was-here\n").unwrap();

        assert!(
            until(|| recorder.printed(&id).contains("ginka-was-here")),
            "the shell's output never arrived: {:?}",
            recorder.printed(&id)
        );
        terminals.close(&id).unwrap();
    }

    #[test]
    fn a_terminal_starts_in_the_workspace_it_belongs_to() {
        // A terminal that opens somewhere else is a terminal the user has to
        // `cd` in before it is useful.
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let recorder = Arc::new(Recorder::default());
        let terminals = Terminals::new(recorder.clone());

        let id = terminals.open(&worktree, 24, 80).unwrap();
        terminals.write(&id, "pwd\n").unwrap();

        let name = worktree.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            until(|| recorder.printed(&id).contains(&name)),
            "it opened somewhere else: {:?}",
            recorder.printed(&id)
        );
        terminals.close(&id).unwrap();
    }

    #[test]
    fn a_shell_that_exits_says_so_rather_than_going_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let terminals = Terminals::new(recorder.clone());

        let id = terminals.open(dir.path(), 24, 80).unwrap();
        terminals.write(&id, "exit\n").unwrap();

        assert!(
            until(|| recorder.closed(&id)),
            "a terminal with nothing left to type into has to say so"
        );
        terminals.close(&id).unwrap();
    }

    #[test]
    fn closing_a_terminal_stops_its_shell_and_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::new(Arc::new(Recorder::default()));
        let id = terminals.open(dir.path(), 24, 80).unwrap();
        assert_eq!(terminals.count(), 1);

        terminals.close(&id).unwrap();
        assert_eq!(terminals.count(), 0);
        assert!(
            terminals.write(&id, "echo\n").is_err(),
            "there is nothing to type into"
        );
    }

    #[test]
    fn a_terminal_can_be_told_how_big_its_window_is() {
        // Without it a full-screen program lays out for the wrong size.
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let terminals = Terminals::new(recorder.clone());
        let id = terminals.open(dir.path(), 24, 80).unwrap();

        terminals.resize(&id, 40, 132).unwrap();
        terminals.write(&id, "tput cols\n").unwrap();
        assert!(
            until(|| recorder.printed(&id).contains("132")),
            "the shell was not told: {:?}",
            recorder.printed(&id)
        );
        terminals.close(&id).unwrap();
    }

    #[test]
    fn writing_to_a_terminal_that_is_not_there_says_which() {
        let terminals = Terminals::new(Arc::new(Recorder::default()));
        let error = terminals
            .write(&TerminalId("absent".into()), "hello")
            .unwrap_err();
        assert!(error.to_string().contains("absent"), "{error}");
    }
}
