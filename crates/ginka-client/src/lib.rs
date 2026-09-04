//! Client for the Ginka daemon's WebSocket RPC.
//!
//! The GPUI app and the `ginka` CLI both talk to the daemon through this crate
//! and nothing else. That constraint is what makes every UI capability
//! scriptable — see `AGENTS.md` rule 3.
//!
//! The transport lands in M2 (`docs/roadmap.md` §5). For now this crate exists
//! so the dependency edges are established and cannot be routed around later.

pub mod cursor;

pub use cursor::{Delivery, EventCursor};
/// How the app and CLI find a running daemon: `~/.ginka/daemon.json`. The
/// daemon writes it (`ginka_core::daemon`), both clients read it, so the type
/// itself lives in the protocol crate where neither side owns it.
pub use ginka_protocol::handshake::DaemonHandshake as Handshake;

use ginka_protocol::envelope::Seq;

/// Where a reconnecting client resumes the event stream from.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResumeCursor {
    pub after: Seq,
}
