//! How a client finds the daemon.
//!
//! The daemon writes this to `~/.ginka/daemon.json` when it starts listening;
//! the app and the CLI read it to know where to connect and what to
//! authenticate with. The struct lives here because both sides of that split
//! have to agree on it; reading and writing the file does not, and belongs to
//! whichever process owns the directory.

use serde::{Deserialize, Serialize};

/// The contents of `~/.ginka/daemon.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// The loopback port the daemon accepted on. Never a wildcard: the daemon
    /// asks the OS for a free port and publishes the one it got.
    pub port: u16,
    /// Bearer token, presented on the WebSocket upgrade. The file is written
    /// `0600`; the daemon binds loopback only.
    pub token: String,
    /// The daemon's process id, so a client can report which process it is
    /// talking to — and a user can kill it.
    pub pid: u32,
    /// The daemon's version, so a client can refuse a protocol it predates.
    pub version: String,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Handshake {
        Handshake {
            port: 51_234,
            token: "t0ken".into(),
            pid: 42,
            version: "0.0.0".into(),
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
}
