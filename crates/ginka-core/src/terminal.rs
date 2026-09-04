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
use ginka_protocol::model::TerminalInfo;
use ginka_protocol::{DaemonEvent, TerminalId, WorkspaceId};
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

/// How much of a shell's output is kept for a window that comes back.
///
/// The daemon holds the pty, so a window that was closed and reopened has
/// missed everything printed meanwhile; without a replay it reattaches to a
/// blank screen and a prompt it cannot see. A quarter of a megabyte is several
/// screens of a build's output and small enough to keep per terminal.
const HISTORY_LIMIT: usize = 256 * 1024;

/// One running shell.
struct Running {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Dropping this kills the shell.
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// The workspace it was opened in, so a window can find its shells again.
    workspace: WorkspaceId,
    /// What it is called in a tab strip.
    title: String,
    /// What it has printed, trimmed to `HISTORY_LIMIT`.
    ///
    /// Shared with the thread reading the pty rather than passed through the
    /// event sink, because the sink's job is to tell the windows that are here
    /// now and this is for the one that is not.
    history: Arc<Mutex<String>>,
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
    pub fn open(
        &self,
        workspace: &WorkspaceId,
        worktree: &Path,
        rows: u16,
        cols: u16,
    ) -> Result<TerminalId> {
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
        let history = Arc::new(Mutex::new(String::new()));

        self.pump(id.clone(), reader, history.clone());
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        // Numbered within the workspace, because that is the strip they appear
        // in: two shells in two workspaces are both the first one there.
        let ordinal = running
            .values()
            .filter(|other| &other.workspace == workspace)
            .count()
            + 1;
        running.insert(
            id.clone(),
            Running {
                writer,
                master: pair.master,
                child,
                workspace: workspace.clone(),
                title: format!("shell {ordinal}"),
                history,
            },
        );
        Ok(id)
    }

    /// The shells running in a workspace, oldest first.
    ///
    /// What a window asks for when it opens: the daemon kept them running, and
    /// a dock that did not show them would be hiding work that is still going.
    pub fn list(&self, workspace: &WorkspaceId) -> Vec<TerminalInfo> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let mut found: Vec<TerminalInfo> = running
            .iter()
            .filter(|(_, shell)| &shell.workspace == workspace)
            .map(|(id, shell)| TerminalInfo {
                id: id.clone(),
                workspace: shell.workspace.clone(),
                title: shell.title.clone(),
            })
            .collect();
        // By the name they were given, which counts up as they were opened: a
        // HashMap has no order and a tab strip that reshuffled itself would be
        // unusable.
        found.sort_by(|a, b| a.title.cmp(&b.title));
        found
    }

    /// What a terminal has printed lately, for a window that has just found it.
    pub fn history(&self, id: &TerminalId) -> Result<String> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let shell = running
            .get(id)
            .with_context(|| format!("no terminal with id {id}"))?;
        let history = shell.history.lock().unwrap_or_else(|e| e.into_inner());
        Ok(history.clone())
    }

    /// Forward everything the shell prints, until it stops.
    ///
    /// On its own thread rather than the async executor: a pty read is a
    /// blocking file read with no async form on every platform, and one shell
    /// producing output must not occupy a task slot the agents are using.
    fn pump(
        &self,
        id: TerminalId,
        mut reader: Box<dyn std::io::Read + Send>,
        history: Arc<Mutex<String>>,
    ) {
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
                        remember(&history, &data);
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

/// Keep `data` in a terminal's history, dropping the oldest to stay bounded.
///
/// Trimmed on a character boundary rather than a byte one: a screen replayed
/// from half a character starts with a replacement glyph, and the escape
/// sequences around it are what draw the screen.
fn remember(history: &Mutex<String>, data: &str) {
    let mut history = history.lock().unwrap_or_else(|e| e.into_inner());
    history.push_str(data);
    if history.len() > HISTORY_LIMIT {
        let mut cut = history.len() - HISTORY_LIMIT;
        while cut < history.len() && !history.is_char_boundary(cut) {
            cut += 1;
        }
        history.drain(..cut);
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

    /// The workspace the test's shells belong to.
    fn workspace() -> WorkspaceId {
        WorkspaceId("project/branch".into())
    }

    /// Type a line into a terminal and wait for what it should produce,
    /// typing it again until it does.
    ///
    /// Retried rather than sent once, because a shell that has not finished
    /// setting up its tty resets it with `TCSAFLUSH`, which throws away
    /// whatever was typed before it got there. A user retypes the line; a test
    /// that did not would blame the terminal for the shell's start-up race.
    fn typed(terminals: &Terminals, id: &TerminalId, line: &str, check: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            // Ignored: a shell that has already gone is one the check is
            // waiting to hear about, not a reason to fail here.
            terminals.write(id, line).ok();
            let again = Instant::now() + Duration::from_millis(500);
            while Instant::now() < again {
                if check() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        false
    }

    #[test]
    fn a_terminal_runs_a_shell_and_forwards_what_it_prints() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let terminals = Terminals::new(recorder.clone());

        let id = terminals.open(&workspace(), dir.path(), 24, 80).unwrap();

        assert!(
            typed(&terminals, &id, "echo ginka-was-here\n", || recorder
                .printed(&id)
                .contains("ginka-was-here")),
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

        let id = terminals.open(&workspace(), &worktree, 24, 80).unwrap();

        let name = worktree.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            typed(&terminals, &id, "pwd\n", || recorder
                .printed(&id)
                .contains(&name)),
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

        let id = terminals.open(&workspace(), dir.path(), 24, 80).unwrap();

        assert!(
            typed(&terminals, &id, "exit\n", || recorder.closed(&id)),
            "a terminal with nothing left to type into has to say so"
        );
        terminals.close(&id).unwrap();
    }

    #[test]
    fn closing_a_terminal_stops_its_shell_and_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::new(Arc::new(Recorder::default()));
        let id = terminals.open(&workspace(), dir.path(), 24, 80).unwrap();
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
        let id = terminals.open(&workspace(), dir.path(), 24, 80).unwrap();

        terminals.resize(&id, 40, 132).unwrap();
        assert!(
            typed(&terminals, &id, "tput cols\n", || recorder
                .printed(&id)
                .contains("132")),
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

    #[test]
    fn a_window_that_comes_back_is_told_what_it_missed() {
        // The daemon holds the pty, so a reopened window has missed whatever
        // was printed meanwhile; without the replay it reattaches to a blank
        // screen and a prompt it cannot see.
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::default());
        let terminals = Terminals::new(recorder.clone());

        let id = terminals.open(&workspace(), dir.path(), 24, 80).unwrap();
        assert!(typed(&terminals, &id, "echo replayed-me\n", || recorder
            .printed(&id)
            .contains("replayed-me")));

        let history = terminals.history(&id).unwrap();
        assert!(
            history.contains("replayed-me"),
            "the history is what the shell printed: {history:?}"
        );
        terminals.close(&id).unwrap();
        assert!(
            terminals.history(&id).is_err(),
            "a terminal that is gone has no history to replay"
        );
    }

    #[test]
    fn history_is_bounded_and_keeps_the_newest() {
        // A build printing for an hour must not grow the daemon without limit,
        // and what a returning reader wants is the end of it.
        let history = Mutex::new(String::new());
        remember(&history, &"a".repeat(HISTORY_LIMIT));
        remember(&history, "the newest words");
        let kept = history.lock().unwrap();
        assert!(kept.len() <= HISTORY_LIMIT);
        assert!(kept.ends_with("the newest words"));
    }

    #[test]
    fn a_workspaces_shells_are_listed_in_the_order_they_were_opened() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::new(Arc::new(Recorder::default()));
        let mine = workspace();
        let other = WorkspaceId("project/elsewhere".into());

        let first = terminals.open(&mine, dir.path(), 24, 80).unwrap();
        let second = terminals.open(&mine, dir.path(), 24, 80).unwrap();
        let elsewhere = terminals.open(&other, dir.path(), 24, 80).unwrap();

        let listed = terminals.list(&mine);
        assert_eq!(
            listed.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec![first.clone(), second.clone()],
            "one workspace's strip shows that workspace's shells"
        );
        assert_eq!(listed[0].title, "shell 1");
        assert_eq!(listed[1].title, "shell 2");
        assert_eq!(
            terminals.list(&other)[0].title,
            "shell 1",
            "the first shell in a workspace is its first, whatever else is running"
        );

        for id in [first, second, elsewhere] {
            terminals.close(&id).unwrap();
        }
        assert!(terminals.list(&mine).is_empty());
    }
}
