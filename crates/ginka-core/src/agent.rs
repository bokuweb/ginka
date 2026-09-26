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
use ginka_protocol::model::{
    PlanSource, PlanUsage, QueuedMessage, SessionState, TranscriptEntry, TranscriptPayload,
};
use ginka_protocol::{AgentEvent, DaemonEvent, SessionId};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// Provider-emitted binary payloads, owned by the daemon host.
    blobs: crate::blob::BlobStore,
    /// How many checkpoints a workspace keeps, from the daemon's settings.
    checkpoint_limit: u32,
    /// Hold the machine awake while each turn's process lives.
    keep_awake: bool,
}

impl Shared {
    /// Append to the transcript and push the event to every client.
    fn record(&self, session: &SessionId, mut payload: TranscriptPayload) -> Result<u64> {
        if let TranscriptPayload::Agent { event } = &mut payload
            && let Err(error) = self.blobs.externalize_event(event)
        {
            // A malformed provider payload is worth surfacing in the log, but
            // dropping the whole transcript entry would hide more than it
            // protects. The ordinary transcript bounds still apply.
            tracing::warn!(%error, session = %session, "could not externalize provider payload");
            if let AgentEvent::ToolCall { activity } | AgentEvent::ToolResult { activity } = event
                && let Some(detail) = activity.detail.clone()
            {
                activity.complete_with(&detail, activity.failed);
            }
        }
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
            &stored.account,
            stored.model.as_deref(),
            usage,
            now(),
        ) {
            tracing::warn!(%error, session = %session, "could not record what a turn cost");
        }
    }

    /// Keep what the vendor said about the account's rate-limit windows, and
    /// tell every client the gauge moved (`docs/accounts.md` §6).
    ///
    /// Filed against the session's account rather than its agent: the
    /// windows are the login's, and two logins of one provider have two.
    fn record_plan(&self, session: &SessionId, usage: &PlanUsage) {
        let snapshot = {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            let Ok(Some(stored)) = session::get(&conn, session) else {
                return;
            };
            crate::usage::record_plan(&conn, &stored.account, usage, now(), PlanSource::Reported)
        };
        match snapshot {
            Ok(snapshot) => self.events.emit(DaemonEvent::PlanUsageChanged { snapshot }),
            Err(error) => {
                tracing::warn!(%error, session = %session, "could not record the account's windows")
            }
        }
    }

    /// Snapshot the worktree so this point in the transcript can be returned
    /// to.
    ///
    /// A workspace that is not a repository, or one whose directory has gone,
    /// cannot be snapshotted; that is logged and the turn carries on, because
    /// losing the ability to rewind is not a reason to stop an agent.
    fn checkpoint(
        &self,
        session: &SessionId,
        workspace_path: &Path,
        turn: u32,
        label: &str,
        start: Option<&checkpoint::TurnStart>,
    ) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(Some(stored)) = session::get(&conn, session) else {
            return;
        };
        if let Err(error) = checkpoint::take(
            &conn,
            workspace_path,
            checkpoint::TurnRef {
                workspace: &stored.workspace,
                session,
                turn,
            },
            label,
            start,
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
    /// The way into the turn while it is running, for transports that read
    /// their input as they work. Cleared when the turn ends, which closes the
    /// agent's input and lets it exit (§3.3 N1).
    steer: Arc<Mutex<Option<async_channel::Sender<String>>>>,
    /// Interaction ids raised by this turn and not answered yet.
    requests: Arc<Mutex<HashSet<String>>>,
    /// The live transport owns the response encoding contract.
    driver: Arc<dyn AgentDriver>,
    /// Set when the user cancelled, so the exit is not reported as a failure.
    cancelled: Arc<AtomicBool>,
    /// Dropping this would detach the turn; it is kept so the supervisor owns
    /// the task for as long as the process is alive.
    _task: smol::Task<()>,
}

/// One session's editable FIFO of follow-ups.
///
/// The sequence remains monotonic after the queue becomes empty so a stale UI
/// action can never address a later message that happened to reuse an id.
#[derive(Debug, Default)]
struct PendingQueue {
    next_id: u64,
    items: VecDeque<QueuedMessage>,
    /// Held: nothing dispatches until the reader lets it go. A stopped or
    /// failed turn holds it, and so does a daemon restart.
    paused: bool,
    /// The prompt an interrupt is making room for: when the turn it stopped
    /// has exited, this goes next and the queue keeps going.
    interrupt_with: Option<u64>,
}

impl PendingQueue {
    /// Append a message and return the stable row clients address.
    fn push(&mut self, text: String) -> QueuedMessage {
        self.next_id = self.next_id.saturating_add(1);
        let message = QueuedMessage {
            id: self.next_id,
            text,
        };
        self.items.push_back(message.clone());
        message
    }

    /// Edit one queued prompt without moving it.
    fn edit(&mut self, id: u64, text: String) -> Result<()> {
        let message = self
            .items
            .iter_mut()
            .find(|message| message.id == id)
            .with_context(|| format!("queued message {id} does not exist"))?;
        message.text = text;
        Ok(())
    }

    /// Remove one prompt, returning what was removed.
    fn remove(&mut self, id: u64) -> Result<QueuedMessage> {
        let index = self
            .items
            .iter()
            .position(|message| message.id == id)
            .with_context(|| format!("queued message {id} does not exist"))?;
        Ok(self.items.remove(index).expect("the index was just found"))
    }

    /// Move a prompt to a zero-based position, clamped to the queue's end.
    fn move_to(&mut self, id: u64, index: usize) -> Result<()> {
        let message = self.remove(id)?;
        let index = index.min(self.items.len());
        self.items.insert(index, message);
        Ok(())
    }

    /// Remove the next prompt to dispatch.
    fn pop_front(&mut self) -> Option<QueuedMessage> {
        self.items.pop_front()
    }

    /// What happens to the queue when its turn stops or fails: the prompt an
    /// interrupt made room for goes to the front and the queue keeps going;
    /// otherwise it is held rather than thrown away. Answers whether the
    /// front should be dispatched now.
    fn after_stop(&mut self) -> bool {
        match self.interrupt_with.take() {
            Some(id) if self.move_to(id, 0).is_ok() => {
                self.paused = false;
                true
            }
            _ => {
                if !self.items.is_empty() {
                    self.paused = true;
                }
                false
            }
        }
    }

    /// Current rows in dispatch order.
    fn items(&self) -> Vec<QueuedMessage> {
        self.items.iter().cloned().collect()
    }
}

/// Write one session's queue to the database, replacing what was there.
///
/// The whole queue each time: it is a handful of rows, and a rewrite cannot
/// leave positions out of step with a move that half-happened.
fn save_queue(conn: &Connection, session: &SessionId, queue: &PendingQueue) -> Result<()> {
    conn.execute(
        "DELETE FROM queued_messages WHERE session_id = ?1",
        [&session.0],
    )?;
    for (position, message) in queue.items.iter().enumerate() {
        conn.execute(
            "INSERT INTO queued_messages (session_id, id, position, text) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session.0, message.id as i64, position as i64, message.text],
        )?;
    }
    conn.execute(
        "INSERT INTO queue_state (session_id, next_id, paused) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET next_id = ?2, paused = ?3",
        rusqlite::params![session.0, queue.next_id as i64, queue.paused],
    )?;
    Ok(())
}

/// Every stored queue, as a daemon that has just started finds them.
///
/// A queue with anything in it comes back held: its turns died with the old
/// daemon, and firing a prompt into a session nobody is watching the moment
/// the daemon returns would be a surprise.
fn load_queues(conn: &Connection) -> Result<HashMap<SessionId, PendingQueue>> {
    let mut queues: HashMap<SessionId, PendingQueue> = HashMap::new();
    let mut state = conn.prepare("SELECT session_id, next_id FROM queue_state")?;
    for row in state.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (session, next_id) = row?;
        queues.entry(SessionId(session)).or_default().next_id = next_id.max(0) as u64;
    }
    let mut items = conn.prepare(
        "SELECT session_id, id, text FROM queued_messages ORDER BY session_id, position",
    )?;
    for row in items.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (session, id, text) = row?;
        let queue = queues.entry(SessionId(session)).or_default();
        let id = id.max(0) as u64;
        queue.next_id = queue.next_id.max(id);
        queue.items.push_back(QueuedMessage { id, text });
    }
    for queue in queues.values_mut() {
        queue.paused = !queue.items.is_empty();
    }
    Ok(queues)
}

/// Store a session's queue and tell every client it moved.
fn queue_changed_in(
    context: &Shared,
    queued: &Mutex<HashMap<SessionId, PendingQueue>>,
    session: &SessionId,
) {
    {
        let queues = queued.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(queue) = queues.get(session) {
            let conn = context
                .conn
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Err(error) = save_queue(&conn, session, queue) {
                tracing::warn!(%error, %session, "could not store the queue");
            }
        }
    }
    context.events.emit(DaemonEvent::SessionQueueChanged {
        session: session.clone(),
    });
}

/// Owns every running agent process.
///
/// One per daemon. It is driven from `Service`, which is itself serialised, so
/// its own locks are only held long enough to look a session up.
pub struct Supervisor {
    context: Shared,
    running: Arc<Mutex<HashMap<SessionId, Running>>>,
    /// Follow-ups that arrived while a turn was still going.
    queued: Arc<Mutex<HashMap<SessionId, PendingQueue>>>,
}

impl Supervisor {
    /// Build a supervisor over the daemon's database and event sink.
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        events: Arc<dyn EventSink>,
        checkpoint_limit: u32,
        blobs: crate::blob::BlobStore,
    ) -> Self {
        let stored = load_queues(&conn.lock().unwrap_or_else(|error| error.into_inner()))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not read the stored queues");
                HashMap::new()
            });
        Self {
            context: Shared {
                conn,
                events,
                blobs,
                checkpoint_limit,
                keep_awake: false,
            },
            running: Arc::new(Mutex::new(HashMap::new())),
            queued: Arc::new(Mutex::new(stored)),
        }
    }

    /// Keep the machine awake while agents work (`DaemonSettings::keep_awake`).
    pub fn with_keep_awake(mut self, keep_awake: bool) -> Self {
        self.context.keep_awake = keep_awake;
        self
    }

    /// Whether a process is alive for this session.
    pub fn is_running(&self, session: &SessionId) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(session)
    }

    /// Follow-ups waiting behind this session's current turn.
    pub fn queued_messages(&self, session: &SessionId) -> Vec<QueuedMessage> {
        self.queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session)
            .map(PendingQueue::items)
            .unwrap_or_default()
    }

    /// Whether this session's active transport can receive a queued prompt.
    pub fn can_send_queued_message_now(&self, session: &SessionId) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session)
            .is_some_and(|entry| {
                entry.driver.supports_steer()
                    && entry
                        .steer
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .is_some()
            })
    }

    /// Replace one queued prompt without changing its dispatch position.
    pub fn edit_queued_message(&self, session: &SessionId, id: u64, text: String) -> Result<()> {
        anyhow::ensure!(!text.trim().is_empty(), "a queued message cannot be blank");
        self.queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(session)
            .with_context(|| format!("session {session} has no queued messages"))?
            .edit(id, text)?;
        self.queue_changed(session);
        Ok(())
    }

    /// Remove one queued prompt before it reaches the transcript.
    pub fn remove_queued_message(&self, session: &SessionId, id: u64) -> Result<()> {
        self.queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(session)
            .with_context(|| format!("session {session} has no queued messages"))?
            .remove(id)?;
        self.queue_changed(session);
        Ok(())
    }

    /// Move one queued prompt to a zero-based dispatch position.
    pub fn move_queued_message(&self, session: &SessionId, id: u64, index: usize) -> Result<()> {
        self.queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(session)
            .with_context(|| format!("session {session} has no queued messages"))?
            .move_to(id, index)?;
        self.queue_changed(session);
        Ok(())
    }

    /// Inject one queued prompt into the current turn.
    ///
    /// Transports without live unsolicited input refuse this operation and
    /// leave the item in place; stopping useful work is never an implicit
    /// consequence of pressing "send now".
    pub fn send_queued_message_now(&self, session: &SessionId, id: u64) -> Result<()> {
        let (sender, driver) = {
            let running = self
                .running
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let entry = running
                .get(session)
                .with_context(|| format!("session {session} has no running turn"))?;
            let sender = entry
                .steer
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .context("the running transport cannot receive a message now")?;
            (sender, entry.driver.clone())
        };
        let mut queues = self
            .queued
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let queue = queues
            .get_mut(session)
            .with_context(|| format!("session {session} has no queued messages"))?;
        let message = queue
            .items
            .iter()
            .find(|message| message.id == id)
            .cloned()
            .with_context(|| format!("queued message {id} does not exist"))?;
        let line = driver
            .encode_user_message(&message.text)
            .context("the running transport cannot receive a message now")?;
        sender
            .try_send(line)
            .context("the running transport stopped before receiving the message")?;
        queue.remove(id)?;
        drop(queues);
        self.context
            .record(session, TranscriptPayload::User { text: message.text })?;
        self.queue_changed(session);
        Ok(())
    }

    /// Store one queue mutation and announce it; the ordered rows remain
    /// daemon-owned.
    fn queue_changed(&self, session: &SessionId) {
        queue_changed_in(&self.context, &self.queued, session);
    }

    /// Whether this session's queue is held.
    pub fn queue_paused(&self, session: &SessionId) -> bool {
        self.queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session)
            .is_some_and(|queue| queue.paused)
    }

    /// How many follow-ups wait in this session's queue.
    pub fn queued_count(&self, session: &SessionId) -> u32 {
        self.queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session)
            .map(|queue| queue.items.len() as u32)
            .unwrap_or(0)
    }

    /// Put a follow-up at the back of the queue, whatever the transport
    /// could do with it now — the Codex CLI's Tab.
    pub fn enqueue(&self, session: &SessionId, text: String) -> Result<QueuedMessage> {
        anyhow::ensure!(!text.trim().is_empty(), "a queued message cannot be blank");
        let message = self
            .queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(session.clone())
            .or_default()
            .push(text);
        self.queue_changed(session);
        Ok(message)
    }

    /// Whether a new follow-up would wait behind something: a turn running,
    /// a held queue, or prompts already waiting.
    pub fn would_wait(&self, session: &SessionId) -> bool {
        self.is_running(session)
            || self
                .queued
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(session)
                .is_some_and(|queue| queue.paused || !queue.items.is_empty())
    }

    /// Hold the queue or let it go. Answers whether the caller should
    /// dispatch the front now: let go, nothing running, something waiting.
    pub fn set_queue_paused(&self, session: &SessionId, paused: bool) -> bool {
        let dispatch = {
            let mut queues = self
                .queued
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let queue = queues.entry(session.clone()).or_default();
            queue.paused = paused;
            !paused && !queue.items.is_empty()
        };
        self.queue_changed(session);
        dispatch && !self.is_running(session)
    }

    /// Throw away every waiting follow-up.
    pub fn clear_queue(&self, session: &SessionId) {
        if let Some(queue) = self
            .queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(session)
        {
            queue.items.clear();
            queue.interrupt_with = None;
            queue.paused = false;
        }
        self.queue_changed(session);
    }

    /// Take the front of the queue to send now, for a caller that has made
    /// sure nothing is running.
    pub fn take_front(&self, session: &SessionId) -> Option<QueuedMessage> {
        let message = self
            .queued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(session)
            .and_then(PendingQueue::pop_front);
        if message.is_some() {
            self.queue_changed(session);
        }
        message
    }

    /// Stop the running turn and send this prompt next — opencodex's Steer.
    ///
    /// The prompt moves to the front at once, so a reader looking at the
    /// queue sees what is about to happen. Answers `true` when a turn was
    /// stopped (the exit hands over), `false` when nothing was running and
    /// the caller should send the front itself.
    pub fn interrupt_with_queued(&self, session: &SessionId, id: u64) -> Result<bool> {
        let running = self.is_running(session);
        {
            let mut queues = self
                .queued
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let queue = queues
                .get_mut(session)
                .with_context(|| format!("session {session} has no queued messages"))?;
            queue.move_to(id, 0)?;
            queue.paused = false;
            if running {
                queue.interrupt_with = Some(id);
            }
        }
        self.queue_changed(session);
        if running {
            self.stop_process(session);
        }
        Ok(running)
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
            // Turn zero *is* the starting state; there is nothing earlier to
            // have been handed.
            None,
        );
        self.run_turn(session, driver, spec, None);
        Ok(())
    }

    /// Send a follow-up.
    ///
    /// While a turn is running a capable transport receives the message in
    /// place; otherwise the editable queue owns it until dispatch. Queued
    /// text enters the immutable transcript only when its turn starts.
    pub fn send(
        &self,
        session: SessionId,
        driver: Arc<dyn AgentDriver>,
        spec: SessionSpec,
        vendor_session_id: Option<String>,
    ) -> Result<()> {
        if self.is_running(&session) {
            // Into the running turn where the transport can take it; the queue
            // is what happens when it cannot (§3.3 N1).
            if let Some(line) = driver.encode_user_message(&spec.prompt) {
                let sender = self
                    .running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&session)
                    .and_then(|entry| {
                        entry
                            .steer
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone()
                    });
                if let Some(sender) = sender
                    && sender.try_send(line).is_ok()
                {
                    self.context.record(
                        &session,
                        TranscriptPayload::User {
                            text: spec.prompt.clone(),
                        },
                    )?;
                    return Ok(());
                }
            }
            self.queued
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(session.clone())
                .or_default()
                .push(spec.prompt);
            self.queue_changed(&session);
            return Ok(());
        }
        self.context.record(
            &session,
            TranscriptPayload::User {
                text: spec.prompt.clone(),
            },
        )?;
        self.run_turn(session, driver, spec, vendor_session_id);
        Ok(())
    }

    /// Start the provider's manual compaction operation as its own turn.
    pub fn compact(
        &self,
        session: SessionId,
        driver: Arc<dyn AgentDriver>,
        spec: SessionSpec,
        vendor_session_id: &str,
    ) -> Result<()> {
        anyhow::ensure!(!self.is_running(&session), "the session is still working");
        let compact = driver
            .compaction(&spec, vendor_session_id)
            .context("the provider does not support manual context compaction")?;
        self.run_process(
            session,
            driver,
            spec,
            compact.command,
            compact.input,
            ParseState::default(),
        );
        Ok(())
    }

    /// Answer a question, plan or permission request inside a running turn.
    ///
    /// The request id is checked before anything is written, so clicking a
    /// stale card can never turn into an unrelated follow-up.
    pub fn respond(&self, session: &SessionId, request_id: &str, response: &str) -> Result<()> {
        let (sender, requests, driver) = {
            let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            let entry = running
                .get(session)
                .with_context(|| format!("session {session} has no running turn"))?;
            let sender = entry
                .steer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .context("the running transport cannot receive responses")?;
            (sender, entry.requests.clone(), entry.driver.clone())
        };

        if !requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(request_id)
        {
            anyhow::bail!("request {request_id} is not waiting for an answer");
        }
        let line = driver
            .encode_response(request_id, response)
            .context("the running transport cannot encode responses")?;
        let state = {
            let mut open = requests.lock().unwrap_or_else(|e| e.into_inner());
            open.remove(request_id);
            if open.is_empty() {
                SessionState::Running
            } else {
                SessionState::AwaitingInput
            }
        };
        self.context.set_state(session, state, None);
        if let Err(error) = sender.try_send(line) {
            requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(request_id.to_string());
            self.context
                .set_state(session, SessionState::AwaitingInput, None);
            return Err(error)
                .context("the running transport stopped before receiving the response");
        }
        self.context.record(
            session,
            TranscriptPayload::Response {
                request_id: request_id.to_string(),
                text: response.to_string(),
            },
        )?;
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
        // A queued follow-up must not start after a cancel: the queue is
        // held, not thrown away — the reader stopped this turn, not the
        // prompts they lined up after it.
        let held = self
            .queued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(session)
            .is_some_and(|queue| {
                queue.interrupt_with = None;
                let had_messages = !queue.items.is_empty();
                if had_messages {
                    queue.paused = true;
                }
                had_messages
            });
        if held {
            self.queue_changed(session);
        }

        match pid {
            Some(pid) => stop_process_tree(pid),
            None => self
                .context
                .set_state(session, SessionState::Cancelled, None),
        }
    }

    /// Kill a running turn's process tree without touching its queue: what
    /// an interrupt does, whose exit hands over to the prompt it chose.
    fn stop_process(&self, session: &SessionId) {
        let pid = {
            let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.get(session).and_then(|entry| {
                entry.cancelled.store(true, Ordering::SeqCst);
                entry.pid
            })
        };
        if let Some(pid) = pid {
            stop_process_tree(pid);
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
        let mut parse = ParseState::default();
        let opening = driver.begin(&spec, vendor_session_id.as_deref(), &mut parse);
        self.run_process(session, driver, spec, command, opening, parse);
    }

    /// Spawn one ordinary or provider-control turn and the task that pumps it.
    fn run_process(
        &self,
        session: SessionId,
        driver: Arc<dyn AgentDriver>,
        spec: SessionSpec,
        command: CommandSpec,
        initial_input: Vec<String>,
        parse: ParseState,
    ) {
        let context = self.context.clone();
        let running = self.running.clone();
        let queued = self.queued.clone();
        let cancelled = Arc::new(AtomicBool::new(false));

        let mut child = match spawn(
            &command,
            &spec,
            driver.supports_steer() || driver.supports_responses() || !initial_input.is_empty(),
        ) {
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
        if context.keep_awake {
            hold_awake(child.id());
        }

        // Transports that read their input as they work are given the prompt
        // that way too, and stay open for whatever is steered in after it. The
        // channel is the supervisor's end of that pipe; dropping it closes the
        // agent's input, which is how the turn is ended.
        let steer: Arc<Mutex<Option<async_channel::Sender<String>>>> = Arc::new(Mutex::new(None));
        if (driver.supports_steer() || driver.supports_responses() || !initial_input.is_empty())
            && let Some(mut stdin) = child.stdin.take()
        {
            let (sender, lines) = async_channel::unbounded::<String>();
            for line in initial_input {
                let _ = sender.try_send(line);
            }
            if driver.supports_steer()
                && let Some(first) = driver.encode_user_message(&spec.agent_prompt())
            {
                let _ = sender.try_send(first);
            }
            *steer.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
            smol::spawn(async move {
                use futures_lite::AsyncWriteExt as _;
                while let Ok(line) = lines.recv().await {
                    if stdin.write_all(line.as_bytes()).await.is_err()
                        || stdin.write_all(b"\n").await.is_err()
                        || stdin.flush().await.is_err()
                    {
                        break;
                    }
                }
                // The turn is over: closing the input is what lets the agent
                // finish rather than wait for a message that is not coming.
                drop(stdin);
            })
            .detach();
        }
        let requests = Arc::new(Mutex::new(HashSet::new()));
        let response_driver = driver.clone();

        let task = smol::spawn({
            let session = session.clone();
            let cancelled = cancelled.clone();
            let workspace_path = spec.workspace_path.clone();
            // A turn is one process, so the driver's own counter restarts with
            // it; the session's count is what the transcript is numbered by.
            let turns_so_far = self.context.turns_completed(&session);
            let steer = steer.clone();
            let requests = requests.clone();
            async move {
                let state = pump(
                    &context,
                    driver.as_ref(),
                    child,
                    Turn {
                        session: &session,
                        workspace_path: &workspace_path,
                        turns_so_far,
                        provider: driver.id(),
                        model: spec.model.as_deref(),
                        reasoning_effort: spec.reasoning_effort.as_deref(),
                        service_tier: spec.service_tier.as_deref(),
                        cancelled: &cancelled,
                        steer: &steer,
                        requests: &requests,
                        parse,
                    },
                )
                .await;
                running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&session);

                // A turn that ended cleanly hands over to whatever the user
                // sent while it was working.
                // A stopped or failed turn holds its queue — unless the stop
                // was an interrupt, which hands over to the prompt it chose.
                if state == SessionState::Cancelled || state == SessionState::Failed {
                    let hand_over = queued
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_mut(&session)
                        .is_some_and(PendingQueue::after_stop);
                    queue_changed_in(&context, &queued, &session);
                    if !hand_over {
                        return;
                    }
                }
                let next = {
                    let mut queues = queued.lock().unwrap_or_else(|e| e.into_inner());
                    match queues.get_mut(&session) {
                        Some(queue) => {
                            // A turn that finished before an interrupt reached
                            // it leaves no interrupt pending: the prompt it
                            // chose is at the front and goes next anyway.
                            queue.interrupt_with = None;
                            if queue.paused {
                                None
                            } else {
                                queue.pop_front()
                            }
                        }
                        None => None,
                    }
                };
                if let Some(message) = next {
                    if context
                        .record(
                            &session,
                            TranscriptPayload::User {
                                text: message.text.clone(),
                            },
                        )
                        .is_err()
                    {
                        context.set_state(
                            &session,
                            SessionState::Failed,
                            Some("could not record the queued follow-up"),
                        );
                        return;
                    }
                    queue_changed_in(&context, &queued, &session);
                    let supervisor = Supervisor {
                        context: context.clone(),
                        running,
                        queued,
                    };
                    let stored = {
                        let conn = context.conn.lock().unwrap_or_else(|e| e.into_inner());
                        session::get(&conn, &session).ok().flatten()
                    };
                    let mut next_spec = spec.clone();
                    next_spec.prompt = message.text;
                    // Whatever the first turn was told, this one was not
                    // forked from anywhere.
                    next_spec.preamble = None;
                    // An option change may have landed while this turn was
                    // running. The queued turn is built from the authoritative
                    // session row rather than the previous process's flags.
                    if let Some(stored) = &stored {
                        next_spec.model = stored.model.clone();
                        next_spec.reasoning_effort = stored.reasoning_effort.clone();
                        next_spec.service_tier = stored.service_tier.clone();
                        next_spec.access_mode = stored.access_mode;
                    }
                    let vendor = stored.and_then(|stored| stored.vendor_session_id);
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
                    steer,
                    requests,
                    driver: response_driver,
                    cancelled,
                    _task: task,
                },
            );
    }
}

/// Read one process to its end, recording everything it said.
/// What one turn of one session runs with.
///
/// Grouped rather than passed as six positional arguments: they travel
/// together and a call site reads better naming them than counting them.
struct Turn<'a> {
    session: &'a SessionId,
    workspace_path: &'a Path,
    /// Turns already completed, so the reader's own per-process count carries
    /// on rather than restarting at one.
    turns_so_far: u32,
    /// Effective options recorded before vendor output so provenance exists
    /// for every driver and survives later option changes.
    provider: &'a str,
    model: Option<&'a str>,
    reasoning_effort: Option<&'a str>,
    service_tier: Option<&'a str>,
    /// Set when the user cancelled, so the exit is not read as a crash.
    cancelled: &'a AtomicBool,
    /// The way into the turn while it runs, cleared when it ends.
    steer: &'a Mutex<Option<async_channel::Sender<String>>>,
    /// Interaction ids this turn is blocked on.
    requests: &'a Mutex<HashSet<String>>,
    /// The reader's state as [`AgentDriver::begin`] left it.
    parse: ParseState,
}

async fn pump(
    context: &Shared,
    driver: &dyn AgentDriver,
    mut child: smol::process::Child,
    turn: Turn<'_>,
) -> SessionState {
    let Turn {
        session,
        workspace_path,
        turns_so_far,
        provider,
        model,
        reasoning_effort,
        service_tier,
        cancelled,
        steer,
        requests,
        parse,
    } = turn;
    context.set_state(session, SessionState::Running, None);
    if let Err(error) = context.record(
        session,
        TranscriptPayload::Agent {
            event: AgentEvent::TurnStarted {
                provider: Some(provider.to_string()),
                model: model.map(str::to_string),
                reasoning_effort: reasoning_effort.map(str::to_string),
                service_tier: service_tier.map(str::to_string),
            },
        },
    ) {
        tracing::error!(%error, "could not record turn provenance");
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut parse = ParseState {
        turn: turns_so_far,
        ..parse
    };
    // Captured before the agent runs, so a file the user edited in the terminal
    // between turns counts as part of what the agent was handed rather than as
    // part of what it did (§3.3 N8). Failing to take it is not a reason to stop
    // a turn, so it is logged and the turn carries on without it.
    let turn_start = match checkpoint::begin(workspace_path, session, turns_so_far + 1) {
        Ok(start) => Some(start),
        Err(error) => {
            tracing::warn!(%error, session = %session, "could not capture the turn's starting state");
            None
        }
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
                        // Nothing follows a result: close the agent's input
                        // so a transport that waits on it can exit.
                        steer.lock().unwrap_or_else(|e| e.into_inner()).take();
                    }
                    AgentEvent::TextDelta { text } if !text.trim().is_empty() => {
                        last_text.push_str(text);
                    }
                    // Cumulative for the session as the vendor reports it, so
                    // it is filed against the turn it was current at.
                    AgentEvent::Usage { usage } => {
                        context.record_usage(session, parse.turn + 1, usage);
                    }
                    // A gauge, not conversation: kept per account and pushed,
                    // never written into the transcript.
                    AgentEvent::PlanUsage { usage } => {
                        context.record_plan(session, usage);
                        continue;
                    }
                    AgentEvent::AskUser { id, .. }
                    | AgentEvent::PlanProposal { id, .. }
                    | AgentEvent::Permission { id, .. } => {
                        requests
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id.clone());
                        context.set_state(session, SessionState::AwaitingInput, None);
                    }
                    AgentEvent::TurnEnd { turn } => {
                        // Nothing more can be steered into a turn that has
                        // ended, and dropping the sender closes the agent's
                        // input so it can exit.
                        steer.lock().unwrap_or_else(|e| e.into_inner()).take();
                        requests.lock().unwrap_or_else(|e| e.into_inner()).clear();
                        let label = if last_text.trim().is_empty() {
                            format!("turn {turn}")
                        } else {
                            last_text.clone()
                        };
                        context.checkpoint(
                            session,
                            workspace_path,
                            *turn,
                            &label,
                            turn_start.as_ref(),
                        );
                        last_text.clear();
                    }
                    _ => {}
                }
                if let Err(error) = context.record(session, TranscriptPayload::Agent { event }) {
                    tracing::error!(%error, "could not record an agent event");
                }
            }
            // What the driver has to say back, before the next line is read.
            if !parse.outbox.is_empty() {
                let sender = steer.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for reply in parse.outbox.drain(..) {
                    if let Some(sender) = &sender {
                        let _ = sender.try_send(reply);
                    }
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
    // The user's full search path, so a CLI that is a script finds its
    // interpreter however the daemon was started (`tool_path`).
    crate::tool_path::apply(process);
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
fn spawn(
    command: &CommandSpec,
    spec: &SessionSpec,
    streamed_input: bool,
) -> Result<smol::process::Child> {
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
        // Piped only for the transports that read as they work: an agent that
        // takes its prompt on the command line should see a closed input
        // rather than one that never says anything.
        .stdin(if streamed_input {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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

    #[test]
    fn queued_messages_can_be_edited_removed_and_reordered_by_stable_id() {
        let mut queue = PendingQueue::default();
        let first = queue.push("first".into());
        let second = queue.push("second".into());
        let third = queue.push("third".into());

        queue.edit(second.id, "edited".into()).unwrap();
        queue.move_to(third.id, 0).unwrap();
        assert_eq!(
            queue
                .items()
                .iter()
                .map(|message| (message.id, message.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (third.id, "third"),
                (first.id, "first"),
                (second.id, "edited")
            ]
        );

        assert_eq!(queue.remove(first.id).unwrap().text, "first");
        assert_eq!(
            queue
                .items()
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![third.id, second.id]
        );
    }

    #[test]
    fn a_stale_queue_id_never_changes_another_message() {
        let mut queue = PendingQueue::default();
        let removed = queue.push("old".into());
        queue.remove(removed.id).unwrap();
        let current = queue.push("current".into());

        assert!(queue.edit(removed.id, "wrong".into()).is_err());
        assert!(queue.move_to(removed.id, 0).is_err());
        assert_eq!(queue.items(), vec![current]);
    }

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

/// The command that keeps the machine awake for as long as `pid` lives.
///
/// `-i` holds idle sleep off and nothing more: the display may still sleep,
/// and a closed lid still wins. `-w` ties it to the agent, so it ends by
/// itself the moment the turn does — there is nothing to clean up.
pub fn keep_awake_command(pid: u32) -> Option<(String, Vec<String>)> {
    cfg!(target_os = "macos").then(|| {
        (
            "caffeinate".to_string(),
            vec!["-i".to_string(), "-w".to_string(), pid.to_string()],
        )
    })
}

/// Start `caffeinate` beside a turn's process, best-effort: a machine that
/// sleeps is an inconvenience, not a reason to fail the turn.
fn hold_awake(pid: u32) {
    if let Some((program, args)) = keep_awake_command(pid) {
        match std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            // Reaped on its own thread when the agent exits and it follows.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => tracing::debug!(%error, "could not keep the machine awake"),
        }
    }
}

#[cfg(test)]
mod keep_awake_tests {
    use super::*;

    #[test]
    fn keeping_awake_is_tied_to_the_agent_and_only_on_macos() {
        let command = keep_awake_command(4242);
        assert_eq!(command.is_some(), cfg!(target_os = "macos"));
        if let Some((program, args)) = command {
            assert_eq!(program, "caffeinate");
            assert_eq!(args, vec!["-i", "-w", "4242"]);
        }
    }
}
