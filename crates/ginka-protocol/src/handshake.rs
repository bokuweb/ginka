//! How a client finds the daemon.
//!
//! The daemon writes this to `~/.ginka/daemon.json` when it starts listening;
//! the app and the CLI read it to know where to connect and what to
//! authenticate with. The struct lives here because both sides of that split
//! have to agree on it; reading and writing the file does not, and belongs to
//! whichever process owns the directory.

use serde::{Deserialize, Serialize};

/// Overrides discovery with an explicit `host:port`. This is what makes a
/// daemon running outside the app testable at all.
pub const DAEMON_ADDRESS_ENV: &str = "GINKA_DAEMON_ADDRESS";
/// Overrides the token that would have come from the handshake file.
pub const DAEMON_TOKEN_ENV: &str = "GINKA_DAEMON_TOKEN";

/// The contents of `~/.ginka/daemon.json`.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// The wire contract the daemon speaks. A number rather than the build
    /// version below, because that is what a client can actually compare.
    pub protocol_version: u32,
    /// The loopback port the daemon accepted on. Never a wildcard: the daemon
    /// asks the OS for a free port and publishes the one it got.
    pub port: u16,
    /// Bearer token, presented on the WebSocket upgrade. The file is written
    /// `0600`; the daemon binds loopback only.
    pub token: String,
    /// The daemon's process id, so a client can report which process it is
    /// talking to — and a user can kill it.
    pub pid: u32,
    /// The daemon's build version, for reporting which process is running.
    pub version: String,
    /// Identifies this daemon *run*. Event sequence numbers are per run, so a
    /// client that reconnects across a restart must resync rather than resume.
    pub epoch: u64,
}

impl Handshake {
    /// The WebSocket endpoint to connect to.
    pub fn endpoint(&self) -> String {
        format!("ws://127.0.0.1:{}/rpc", self.port)
    }

    /// The value of the `Authorization` header a client must send.
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Whether this build and that daemon agree about the wire.
    pub fn speaks_our_protocol(&self) -> bool {
        self.protocol_version == crate::envelope::PROTOCOL_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Handshake {
        Handshake {
            protocol_version: crate::envelope::PROTOCOL_VERSION,
            port: 51_234,
            token: "t0ken".into(),
            pid: 42,
            version: "0.0.0".into(),
            epoch: 1,
        }
    }

    #[test]
    fn the_endpoint_is_loopback_only() {
        assert_eq!(handshake().endpoint(), "ws://127.0.0.1:51234/rpc");
    }

    #[test]
    fn the_token_is_presented_as_a_bearer_credential() {
        assert_eq!(handshake().authorization(), "Bearer t0ken");
    }

    #[test]
    fn a_daemon_on_another_contract_is_recognised_as_such() {
        let mut newer = handshake();
        newer.protocol_version = crate::envelope::PROTOCOL_VERSION + 1;
        assert!(!newer.speaks_our_protocol());
        assert!(handshake().speaks_our_protocol());
    }
}
