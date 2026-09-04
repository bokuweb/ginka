//! Running agents: spawning, streaming, cancelling and queueing.
//!
//! Everything here is the same for every vendor. What differs — the command
//! line and the output format — is behind [`AgentDriver`], so this module
//! never learns which agent it is supervising (`AGENTS.md` rule 6).
//!
//! A turn is one process. Vendors' non-interactive modes exit when the turn
//! ends, so a follow-up is a fresh process resuming the vendor's session
//! rather than a write to a still-open stdin. That is why the vendor session
//! id is worth persisting: without it a conversation can be replayed but not
//! continued.

use crate::driver::{AgentDriver, CommandSpec, ParseState, SessionSpec};
use crate::service::EventSink;
use crate::{checkpoint, session};
use anyhow::{Context, Result};
use futures_lite::io::BufReader;
use futures_lite::{AsyncBufReadExt, StreamExt};
use ginka_protocol::model::{SessionState, TranscriptEntry, TranscriptPayload};
use ginka_protocol::{AgentEvent, DaemonEvent, SessionId};
use rusqlite::Connection;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How much of an agent's stderr is kept to explain a failure.
///
/// Enough for a stack trace or an authentication message, bounded because a
/// misbehaving agent can produce megabytes of it.
const STDERR_KEPT: usize = 4096;

/// What a running turn shares with the rest of the daemon.
#[derive(Clone)]
struct Shared {
    conn: Arc<Mutex<Connection>>,
    events: Arc<dyn EventSink>,
    /// How many checkpoints a workspace keeps, from the daemon's settings.
    checkpoint_limit: u32,
}

impl Shared {
    /// Append to the transcript and push the event to every client.
    fn record(&self, session: &SessionId, payload: TranscriptPayload) -> Result<u64> {
        let at = now();
        let seq = {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            session::append(&conn, session, &payload, at)?
        };
        // Prompts are pushed as well as events: a window that had to wait for
        // a poll to see what the user just typed reads as a window that lost
        // it.
        self.events.emit(DaemonEvent::SessionEvent {
            session: session.clone(),
            entry: TranscriptEntry { seq, at, payload },
        });
        Ok(seq)
    }

    /// How many turns this session has already run.
    fn turns_completed(&self, session: &SessionId) -> u32 {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        session::turns_completed(&conn, session).unwrap_or(0)
    }

    /// Record what a turn had cost by the time the vendor said so.
    fn record_usage(&self, session: &SessionId, turn: u32, usage: &ginka_protocol::Usage) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(Some(stored)) = session::get(&conn, session) else {
            return;
        };
        if let Err(error) = crate::usage::record(
            &conn,
            session,
            turn,
            &stored.agent,
            stored.model.as_deref(),
            usage,
            now(),
        ) {
            tracing::warn!(%error, session = %session, "could not record what a turn cost");
        }
    }

    /// Snapshot the worktree so this point in the transcript can be returned
    /// to.
    ///
    /// A workspace that is not a repository, or one whose directory has gone,
    /// cannot be snapshotted; that is logged and the turn carries on, because
    /// losing the ability to rewind is not a reason to stop an agent.
    fn checkpoint(&self, session: &SessionId, workspace_path: &Path, turn: u32, label: &str) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(Some(stored)) = session::get(&conn, session) else {
            return;
        };
        if let Err(error) = checkpoint::take(
            &conn,
            workspace_path,
            &stored.workspace,
            session,
            turn,
            label,
            now(),
        ) {
            tracing::warn!(%error, session = %session, turn, "could not take a checkpoint");
            return;
        }
        // Here rather than on a timer: a workspace only gains checkpoints by
        // taking one, and this is the moment it just did.
        match checkpoint::prune(
            &conn,
            workspace_path,
            &stored.workspace,
            self.checkpoint_limit,
        ) {
            Ok(dropped) if dropped > 0 => {
                tracing::debug!(
                    dropped,
                    workspace = stored.workspace.0,
                    "pruned old checkpoints"
                )
            }
            Err(error) => tracing::warn!(%error, "could not prune old checkpoints"),
            _ => {}
        }
    }

    /// Move a session, and tell every client it moved.
    ///
    /// The push is what lets a window show that an agent is thinking the
    /// moment it starts, rather than on whatever tick notices next.
    fn set_state(&self, session: &SessionId, state: SessionState, summary: Option<&str>) {
        {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(error) = session::update_state(&conn, session, state, summary, now()) {
                tracing::error!(%error, session = %session, "could not record a session state");
            }
        }
        self.events.emit(DaemonEvent::SessionStateChanged {
            session: session.clone(),
            state,
        });
    }
}

/// One live agent process.
struct Running {
    /// The process group leader's pid, used to stop the whole tree.
    pid: Option<u32>,
    /// Set when the user cancelled, so the exit is not reported as a failure.
    cancelled: Arc<AtomicBool>,
    /// Dropping this would detach the turn; it is kept so the supervisor owns
    /// the task for as long as the process is alive.
    _task: smol::Task<()>,
}

/// Owns every running agent process.
///
/// One per daemon. It is driven from `Service`, which is itself serialised, so
/// its own locks are only held long enough to look a session up.
pub struct Supervisor {
    context: Shared,
    running: Arc<Mutex<HashMap<SessionId, Running>>>,
    /// Follow-ups that arrived while a turn was still going.
    queued: Arc<Mutex<HashMap<SessionId, VecDeque<String>>>>,
}

impl Supervisor {
    /// Build a supervisor over the daemon's database and event sink.
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        events: Arc<dyn EventSink>,
        checkpoint_limit: u32,
    ) -> Self {
        Self {
            context: Shared {
                conn,
                events,
                checkpoint_limit,
            },
            running: Arc::new(Mutex::new(HashMap::new())),
            queued: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether a process is alive for this session.
    pub fn is_running(&self, session: &SessionId) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(session)
    }

    /// Start the first turn of a session, recording the opening prompt.
    pub fn start(
        &self,
        session: SessionId,
        driver: Arc<dyn AgentDriver>,
        spec: SessionSpec,
    ) -> Result<()> {
        self.context.record(
            &session,
            TranscriptPayload::User {
                text: spec.prompt.clone(),
            },
        )?;
        // Before the agent has touched anything, so there is a state to
        // rewind all the way back to.
        self.context.checkpoint(
            &session,
            &spec.workspace_path,
            0,
            &format!("before: {}", spec.prompt),
        );
        self.run_turn(session, driver, spec, None);
        Ok(())
    }

    /// Send a follow-up.
    ///
    /// While a turn is running the message is queued and sent when it ends —
    /// interrupting the agent to deliver it would throw away the work in
    /// progress. The prompt is recorded immediately either way, so the user
    /// sees what they sent in the transcript rather than a message that
    /// vanishes until the agent is free.
    pub fn send(
        &self,
        session: SessionId,
        driver: Arc<dyn AgentDriver>,
        spec: SessionSpec,
        vendor_session_id: Option<String>,
    ) -> Result<()> {
        self.context.record(
            &session,
            TranscriptPayload::User {
                text: spec.prompt.clone(),
            },
        )?;

        if self.is_running(&session) {
            self.queued
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(session)
                .or_default()
                .push_back(spec.prompt);
            return Ok(());
        }
        self.run_turn(session, driver, spec, vendor_session_id);
        Ok(())
    }

    /// Stop a session's process tree.
    ///
    /// Marking it cancelled first is what keeps the exit from being reported
    /// as a crash: the process is about to die because we killed it.
    pub fn cancel(&self, session: &SessionId) {
        let pid = {
            let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            match running.get(session) {
                Some(entry) => {
                    entry.cancelled.store(true, Ordering::SeqCst);
                    entry.pid
                }
                None => None,
            }
        };
        // A queued follow-up must not start after a cancel.
        self.queued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);

        match pid {
            Some(pid) => stop_process_tree(pid),
            None => self
                .context
                .set_state(session, SessionState::Cancelled, None),
        }
    }

    /// Spawn one turn and the task that pumps it.
    fn run_turn(
        &self,
        session: SessionId,
        driver: Arc<dyn AgentDriver>,
        spec: SessionSpec,
        vendor_session_id: Option<String>,
    ) {
        let command = match &vendor_session_id {
            Some(vendor) => driver.resume_command(&spec, vendor),
            None => driver.start_command(&spec),
        };

        let context = self.context.clone();
        let running = self.running.clone();
        let queued = self.queued.clone();
        let cancelled = Arc::new(AtomicBool::new(false));

        let child = match spawn(&command, &spec) {
            Ok(child) => child,
            Err(error) => {
                tracing::error!(%error, agent = driver.id(), "could not start the agent");
                // `set_state` publishes the move, so there is nothing else to
                // announce: the failure is the state.
                context.set_state(
                    &session,
                    SessionState::Failed,
                    Some(&format!("could not start {}: {error}", command.program)),
                );
                return;
            }
        };
        let pid = Some(child.id());

        let task = smol::spawn({
            let session = session.clone();
            let cancelled = cancelled.clone();
            let workspace_path = spec.workspace_path.clone();
            // A turn is one process, so the driver's own counter restarts with
            // it; the session's count is what the transcript is numbered by.
            let turns_so_far = self.context.turns_completed(&session);
            async move {
                let state = pump(
                    &context,
                    &session,
                    driver.as_ref(),
                    child,
                    &cancelled,
                    &workspace_path,
                    turns_so_far,
                )
                .await;
                running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&session);

                // A turn that ended cleanly hands over to whatever the user
                // sent while it was working.
                if state == SessionState::Cancelled || state == SessionState::Failed {
                    queued
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&session);
                    return;
                }
                let next = queued
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&session)
                    .and_then(VecDeque::pop_front);
                if let Some(prompt) = next {
                    let supervisor = Supervisor {
                        context: context.clone(),
                        running,
                        queued,
                    };
                    let vendor = {
                        let conn = context.conn.lock().unwrap_or_else(|e| e.into_inner());
                        session::get(&conn, &session)
                            .ok()
                            .flatten()
                            .and_then(|stored| stored.vendor_session_id)
                    };
                    let mut next_spec = spec.clone();
                    next_spec.prompt = prompt;
                    supervisor.run_turn(session, driver, next_spec, vendor);
                }
            }
        });

        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                session,
                Running {
                    pid,
                    cancelled,
                    _task: task,
                },
            );
    }
}

/// Read one process to its end, recording everything it said.
async fn pump(
    context: &Shared,
    session: &SessionId,
    driver: &dyn AgentDriver,
    mut child: smol::process::Child,
    cancelled: &AtomicBool,
    workspace_path: &Path,
    turns_so_far: u32,
) -> SessionState {
    context.set_state(session, SessionState::Running, None);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut parse = ParseState {
        turn: turns_so_far,
        ..ParseState::default()
    };
    let mut reported: Option<(SessionState, Option<String>)> = None;
    // What the agent last said, which is the useful label for the checkpoint
    // taken at the end of the turn.
    let mut last_text = String::new();

    if let Some(stdout) = stdout {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next().await {
            let Ok(line) = line else { break };
            for event in driver.parse_line(&line, &mut parse) {
                match &event {
                    AgentEvent::SessionResult { state, summary } => {
                        reported = Some((*state, summary.clone()));
                    }
                    AgentEvent::TextDelta { text } if !text.trim().is_empty() => {
                        last_text.push_str(text);
                    }
                    // Cumulative for the session as the vendor reports it, so
                    // it is filed against the turn it was current at.
                    AgentEvent::Usage { usage } => {
                        context.record_usage(session, parse.turn + 1, usage);
                    }
                    AgentEvent::TurnEnd { turn } => {
                        let label = if last_text.trim().is_empty() {
                            format!("turn {turn}")
                        } else {
                            last_text.clone()
                        };
                        context.checkpoint(session, workspace_path, *turn, &label);
                        last_text.clear();
                    }
                    _ => {}
                }
                if let Err(error) = context.record(session, TranscriptPayload::Agent { event }) {
                    tracing::error!(%error, "could not record an agent event");
                }
            }
            if let Some(vendor) = parse.vendor_session_id.as_deref() {
                let conn = context.conn.lock().unwrap_or_else(|e| e.into_inner());
                session::set_vendor_session_id(&conn, session, vendor).ok();
            }
        }
    }

    let complaints = match stderr {
        Some(stderr) => read_tail(stderr).await,
        None => String::new(),
    };
    let status = child.status().await;

    if cancelled.load(Ordering::SeqCst) {
        context.set_state(session, SessionState::Cancelled, Some("cancelled"));
        return SessionState::Cancelled;
    }

    let (state, summary) = outcome(reported, &parse, status, &complaints);
    context.set_state(session, state, summary.as_deref());
    state
}

/// Decide how a turn ended.
///
/// The order matters. What the agent said about itself wins; a non-zero exit
/// is next; and a process that exited cleanly but said nothing this driver
/// could read is a failure rather than an empty success — that is what a
/// vendor changing its output format looks like, and reporting it as "done
/// with no transcript" would hide it (`docs/roadmap.md` §7 R6).
fn outcome(
    reported: Option<(SessionState, Option<String>)>,
    parse: &ParseState,
    status: std::io::Result<std::process::ExitStatus>,
    complaints: &str,
) -> (SessionState, Option<String>) {
    if let Some((state, summary)) = reported {
        return (state, summary);
    }
    match status {
        Ok(status) if !status.success() => (
            SessionState::Failed,
            Some(match complaints.trim() {
                "" => format!("the agent exited with {status}"),
                message => message.lines().last().unwrap_or(message).to_string(),
            }),
        ),
        Err(error) => (
            SessionState::Failed,
            Some(format!("could not wait for the agent: {error}")),
        ),
        Ok(_) if parse.understood_nothing() => (
            SessionState::Failed,
            Some(
                "could not understand this agent's output; it may be an unsupported version"
                    .to_string(),
            ),
        ),
        // The process is gone but the conversation can be resumed.
        Ok(_) => (SessionState::Idle, None),
    }
}

/// Whether a variable belongs to the session that started the daemon rather
/// than to the agent it is about to run.
///
/// A daemon is often started from inside another agent's shell — that is how
/// an agent drives Ginka at all — and that shell exports its own session,
/// endpoint and credentials. Passing them on makes every agent behave like
/// whatever happened to launch the daemon: pointed at someone else's gateway,
/// carrying someone else's token, and reporting itself as not logged in.
///
/// A user who does want a gateway or an API key sets it per agent in
/// `settings.json`, which is applied after this and therefore wins. That is
/// the difference between a deliberate choice and an accident of how the
/// daemon was started (`docs/roadmap.md` §6.3).
pub(crate) fn is_inherited_session_state(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "CLAUDE_CODE_",
        "CLAUDE_AGENT_SDK_",
        "CLAUDE_PREVIEW_",
        "CODEX_",
        "GEMINI_",
    ];
    const NAMES: &[&str] = &[
        "CLAUDECODE",
        "CLAUDE_EFFORT",
        "CLAUDE_PID",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "GOOGLE_API_KEY",
    ];
    PREFIXES.iter().any(|prefix| name.starts_with(prefix)) || NAMES.contains(&name)
}

/// Strip the daemon's own inherited session state from a command.
///
/// Shared with the probe, so what an agent is asked about itself is asked in
/// the environment it would actually run in.
pub(crate) fn sanitize(process: &mut std::process::Command) {
    for (name, _) in std::env::vars() {
        if is_inherited_session_state(&name) {
            process.env_remove(&name);
        }
    }
}

/// Start the agent in its workspace.
///
/// The child is put in its own process group so cancelling stops the tools it
/// spawned too, not just the CLI that spawned them. Its stdin is closed: the
/// non-interactive modes this drives read their prompt from the command line,
/// and leaving stdin open makes an agent that expects a terminal hang.
///
/// The environment is sanitized first: see [`is_inherited_session_state`].
fn spawn(command: &CommandSpec, spec: &SessionSpec) -> Result<smol::process::Child> {
    let mut base = std::process::Command::new(&command.program);
    base.args(&command.args).current_dir(&spec.workspace_path);
    sanitize(&mut base);
    // After the sanitizing, so what the user configured wins over what the
    // daemon happened to inherit.
    for (key, value) in command.env.iter().chain(spec.env.iter()) {
        base.env(key, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        base.process_group(0);
    }

    // The pipes are configured through the async command rather than the one
    // it was built from: `async_process` only hands back readable handles for
    // the streams it was itself asked to pipe, so setting them beforehand
    // yields a child with no stdout to read.
    let mut process = smol::process::Command::from(base);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    process
        .spawn()
        .with_context(|| format!("starting {}", command.program))
}

/// Stop an agent, and the tools it started.
///
/// Signalling the group is what makes a cancel reach the compiler an agent
/// started, not only the agent — but only when the agent is that group's
/// leader. A child that did not get its own group is in *ours*, and signalling
/// the group by its pid would either hit nothing or, if that number happened to
/// name a real group, hit something that has nothing to do with this session.
/// So the group is confirmed first and, failing that, the agent alone is
/// stopped: a tool that outlives its agent is a bug worth seeing, and taking
/// the whole session's host down with it is not the way to avoid it.
///
/// Both go through the `kill` binary for the same reason git does: it is the
/// implementation the platform already agrees with, and it keeps this crate
/// free of unsafe.
fn stop_process_tree(pid: u32) {
    // Never signal our own group. On a machine where this process is not a
    // group leader that is the whole session it was started from.
    if pid == 0 || pid == std::process::id() {
        tracing::error!(pid, "refusing to signal this process's own group");
        return;
    }

    let leads_a_group = process_group_of(pid) == Some(pid);
    #[cfg(unix)]
    let target = if leads_a_group {
        format!("-{pid}")
    } else {
        tracing::warn!(pid, "the agent has no group of its own; stopping it alone");
        pid.to_string()
    };
    #[cfg(unix)]
    // `--` because the group form starts with a `-`, which is otherwise an
    // option wherever this runs.
    let killed = std::process::Command::new("kill")
        .args(["-TERM", "--", &target])
        .output();

    #[cfg(windows)]
    let killed = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output();

    if let Err(error) = killed {
        tracing::warn!(%error, pid, "could not stop the agent");
    }
}

/// The process group a pid belongs to.
///
/// Through `ps` rather than `/proc`, which macOS does not have, or `libc`,
/// which this crate does not link.
fn process_group_of(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Read a stream to its end, keeping only the tail.
async fn read_tail(stream: smol::process::ChildStderr) -> String {
    let mut lines = BufReader::new(stream).lines();
    let mut kept = String::new();
    while let Some(Ok(line)) = lines.next().await {
        kept.push_str(&line);
        kept.push('\n');
        if kept.len() > STDERR_KEPT * 2 {
            kept = kept.split_off(kept.len() - STDERR_KEPT);
        }
    }
    kept
}

/// Unix seconds.
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exited(code: i32) -> std::io::Result<std::process::ExitStatus> {
        // A status is only constructible by running something, so run the
        // cheapest thing that can produce the code we need.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .status()?;
        Ok(status)
    }

    #[test]
    fn the_session_that_started_the_daemon_does_not_reach_the_agent() {
        // The failure this prevents: a daemon started from inside another
        // agent's shell hands every agent that shell's endpoint and token, and
        // they all report themselves as not logged in.
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_AGENT_SDK_VERSION",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "CODEX_HOME",
        ] {
            assert!(
                is_inherited_session_state(name),
                "{name} must not be passed on"
            );
        }
    }

    #[test]
    fn the_rest_of_the_environment_is_left_alone() {
        // An agent needs a shell, a home and a terminal like any other program.
        for name in [
            "PATH",
            "HOME",
            "LANG",
            "TERM",
            "SHELL",
            "SSH_AUTH_SOCK",
            "GIT_AUTHOR_NAME",
            "NODE_OPTIONS",
        ] {
            assert!(
                !is_inherited_session_state(name),
                "{name} is the user's own"
            );
        }
    }

    #[test]
    fn a_process_reports_the_group_it_is_in() {
        // The check that decides whether a cancel may signal a group at all.
        let mine = std::process::id();
        assert!(
            process_group_of(mine).is_some(),
            "this process is in some group"
        );
        assert_eq!(
            process_group_of(u32::MAX - 1),
            None,
            "a pid that cannot exist leads nothing"
        );
    }

    #[test]
    fn what_the_agent_said_about_itself_wins() {
        let (state, summary) = outcome(
            Some((SessionState::Finished, Some("done".into()))),
            &ParseState::default(),
            exited(1),
            "noise on stderr",
        );
        assert_eq!(state, SessionState::Finished);
        assert_eq!(summary.as_deref(), Some("done"));
    }

    #[test]
    fn a_non_zero_exit_fails_the_session_and_keeps_the_complaint() {
        let (state, summary) = outcome(
            None,
            &ParseState {
                recognized: 3,
                ..ParseState::default()
            },
            exited(2),
            "Error: not authenticated\n",
        );
        assert_eq!(state, SessionState::Failed);
        assert_eq!(summary.as_deref(), Some("Error: not authenticated"));
    }

    #[test]
    fn a_clean_exit_with_nothing_understood_is_a_failure_not_an_empty_success() {
        let (state, summary) = outcome(
            None,
            &ParseState {
                unrecognized: 5,
                ..ParseState::default()
            },
            exited(0),
            "",
        );
        assert_eq!(state, SessionState::Failed);
        assert!(
            summary.unwrap().contains("unsupported version"),
            "the user has to be told what to fix"
        );
    }

    #[test]
    fn a_clean_exit_after_a_turn_leaves_the_session_resumable() {
        let (state, summary) = outcome(
            None,
            &ParseState {
                recognized: 4,
                ..ParseState::default()
            },
            exited(0),
            "",
        );
        assert_eq!(state, SessionState::Idle);
        assert_eq!(summary, None);
    }
}
