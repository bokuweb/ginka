//! How a client finds the daemon.
//!
//! The daemon publishes `~/.ginka/daemon.json` as it starts listening and
//! removes it as it stops. The file is the whole discovery mechanism: no
//! broadcast, no fixed port, no service registry — one file, readable only by
//! its owner, holding the port and the token. See `docs/roadmap.md` §4.1.

use serde::{Deserialize, Serialize};

/// Overrides discovery with an explicit `host:port`. This is what makes a
/// daemon running outside the app testable at all.
pub const DAEMON_ADDRESS_ENV: &str = "GINKA_DAEMON_ADDRESS";
/// Overrides the token that would have come from the handshake file.
pub const DAEMON_TOKEN_ENV: &str = "GINKA_DAEMON_TOKEN";

/// What the daemon advertises about itself.
///
/// Unknown fields are ignored rather than refused: a client must stay able to
/// read a *newer* daemon's file, or it cannot even report the version
/// mismatch that is the actual problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandshake {
    pub protocol_version: u32,
    pub port: u16,
    /// Bearer token. The file is written 0600 and the daemon binds loopback
    /// only; together those are what stands between a local process and the
    /// user's repositories.
    pub token: String,
    pub pid: u32,
    /// Identifies this daemon *run*. Event sequence numbers are per run, so a
    /// client that reconnects across a restart must resync rather than resume.
    pub epoch: u64,
}

impl DaemonHandshake {
    /// Loopback only. A daemon reachable from the network is a different
    /// product with a different threat model.
    pub fn endpoint(&self) -> String {
        format!("ws://127.0.0.1:{}/rpc", self.port)
    }

    pub fn speaks_our_protocol(&self) -> bool {
        self.protocol_version == crate::envelope::PROTOCOL_VERSION
    }
}
