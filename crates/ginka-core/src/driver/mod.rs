//! The agent driver boundary.
//!
//! Every vendor CLI is reached through one trait and normalizes into one
//! event stream, so nothing above this module knows which agent is running
//! (`AGENTS.md` rule 6). Two behaviours live here rather than in the UI
//! because they are policy, not presentation:
//!
//! * **Steering** ([`FollowUps`]) — a message typed while the agent is working
//!   goes into the running turn where the transport can take it. The queue is
//!   the fallback, not the design. See `docs/roadmap.md` §3.3 N1.
//! * **Option changes** ([`apply_session_options`]) — the driver says whether
//!   it can absorb a change, except for the one change policy decides on its
//!   own. See §3.3 N2.
//!
//! The trait is synchronous. A driver owns a child process and a reader
//! thread, and hands events to the daemon over a channel; making the calls
//! async would buy nothing and cost a second reactor (roadmap §4.3).

pub mod testing;

use anyhow::Result;
use ginka_protocol::provider::{OptionOutcome, SessionOptions};

/// Where a session is in its lifecycle, as far as dispatch is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// The process is starting or the handshake is not finished.
    Connecting,
    /// Connected with no turn running.
    Idle,
    /// A turn is in flight.
    Turn,
}

/// What should happen to a message the user just submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// Empty input; nothing to do.
    Nothing,
    /// Open a new turn with this text.
    StartTurn(String),
    /// Inject into the turn already running.
    Steer(String),
    /// Held until the current turn settles; visible above the composer.
    Queued,
}

/// The composer's dispatch policy for one session.
///
/// Holding the queue here rather than in a view keeps "what happens to this
/// message" testable, and keeps the answer identical in the UI, the CLI and
/// anything else that drives a session.
#[derive(Debug, Default, Clone)]
pub struct FollowUps {
    pending: Vec<String>,
}

impl FollowUps {
    /// Decide what to do with a submitted message.
    ///
    /// `supports_steer` is the *transport's* answer, asked of the live
    /// session: the same provider can support it on one release and not the
    /// next, so it is a parameter rather than a property of the provider.
    pub fn submit(&mut self, message: &str, phase: SessionPhase, supports_steer: bool) -> Dispatch {
        let message = message.trim();
        if message.is_empty() {
            return Dispatch::Nothing;
        }
        match phase {
            SessionPhase::Idle => Dispatch::StartTurn(message.to_string()),
            SessionPhase::Turn if supports_steer => Dispatch::Steer(message.to_string()),
            // Still connecting, or a transport with no way in: hold it where
            // the user can still see it.
            SessionPhase::Connecting | SessionPhase::Turn => {
                self.pending.push(message.to_string());
                Dispatch::Queued
            }
        }
    }

    /// The transport took the steered message; there is nothing left to hold.
    pub fn steer_accepted(&mut self) {}

    /// The transport refused the steered message. It goes to the front of the
    /// queue — ahead of anything typed after it — rather than being lost.
    pub fn steer_rejected(&mut self, message: &str) {
        let message = message.trim();
        if !message.is_empty() {
            self.pending.insert(0, message.to_string());
        }
    }

    /// Called when a turn settles. Everything held opens one turn together:
    /// two follow-ups typed during one turn are one thought, and splitting
    /// them into two turns makes the agent answer the first without the
    /// second.
    pub fn turn_finished(&mut self) -> Option<Dispatch> {
        if self.pending.is_empty() {
            return None;
        }
        let joined = std::mem::take(&mut self.pending).join("\n\n");
        Some(Dispatch::StartTurn(joined))
    }

    /// What the composer shows as still waiting.
    pub fn pending(&self) -> &[String] {
        &self.pending
    }
}

/// One running agent session.
pub trait AgentSession {
    /// Whether a message can be injected into the turn already running.
    fn supports_steer(&self) -> bool;

    /// Inject a message into the running turn. The outcome is asynchronous:
    /// the transport answers with a steer-accepted or steer-rejected event.
    fn steer(&mut self, message: &str) -> Result<()>;

    /// Try to apply new options without restarting, answering whether the
    /// transport managed it.
    fn apply_options(&mut self, options: &SessionOptions) -> Result<OptionOutcome>;

    /// Stop the current turn. Whether the process survives is the driver's
    /// business, not the caller's.
    fn cancel(&mut self) -> Result<()>;
}

/// Apply an option change to a running session, updating `current` when the
/// session took it.
///
/// The access mode never reaches the driver: loosening or tightening what an
/// already-running agent may touch deserves a fresh session even where the
/// transport would accept the change on its next turn.
pub fn apply_session_options(
    session: &mut dyn AgentSession,
    current: &mut SessionOptions,
    next: SessionOptions,
) -> Result<OptionOutcome> {
    if !next.differs_from(current) {
        return Ok(OptionOutcome::Absorbed);
    }
    if SessionOptions::forces_restart(current, &next) {
        return Ok(OptionOutcome::RestartRequired);
    }
    let outcome = session.apply_options(&next)?;
    if outcome.absorbed() {
        *current = next;
    }
    Ok(outcome)
}
