//! Client for the Ginka daemon's WebSocket RPC.
//!
//! The GPUI app and the `ginka` CLI both talk to the daemon through this crate
//! and nothing else. That constraint is what makes every UI capability
//! scriptable — see `AGENTS.md` rule 3.
//!
//! The transport lands in M2 (`docs/roadmap.md` §5). For now this crate exists
//! so the dependency edges are established and cannot be routed around later.

use anyhow::Result;
use ginka_protocol::envelope::Seq;
use std::path::Path;

/// How the app and CLI find a running daemon: `~/.ginka/daemon.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Handshake {
    pub port: u16,
    /// Bearer token. The file is written 0600; the daemon binds loopback only.
    pub token: String,
    pub pid: u32,
    pub version: String,
}

impl Handshake {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn endpoint(&self) -> String {
        format!("ws://127.0.0.1:{}/rpc", self.port)
    }
}

/// Where a reconnecting client resumes the event stream from.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResumeCursor {
    pub after: Seq,
}
