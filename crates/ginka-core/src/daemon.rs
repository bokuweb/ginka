//! Publishing and finding the daemon.
//!
//! One daemon per user, discovered through `~/.ginka/daemon.json`. The file
//! holds the port and a bearer token; it is written with owner-only
//! permissions because anything that can read it can drive every repository
//! the user has registered. See `docs/roadmap.md` §4.1 and §6.3.

use anyhow::{Context, Result};
use ginka_protocol::envelope::{
    ClientMessage, HandshakeRejection, PROTOCOL_VERSION, ServerMessage,
};
use ginka_protocol::handshake::{DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonHandshake};
use uuid::Uuid;

use crate::Paths;

/// Advertise a listening daemon, replacing any previous advertisement.
///
/// The token and epoch are new on every call: a token that outlived its
/// process would authenticate a client to a daemon that no longer exists, and
/// an epoch that repeated would let a stale cursor resume into a fresh event
/// stream.
pub fn publish(paths: &Paths, port: u16) -> Result<DaemonHandshake> {
    let handshake = DaemonHandshake {
        protocol_version: PROTOCOL_VERSION,
        port,
        token: generate_token(),
        pid: std::process::id(),
        epoch: new_epoch(),
    };
    write(paths, &handshake)?;
    Ok(handshake)
}

/// Read the advertisement, if there is one.
///
/// A missing file is the normal "no daemon running" answer. A corrupt one is
/// treated the same way: a daemon killed mid-write must not stop the next one
/// from starting.
pub fn read(paths: &Paths) -> Result<Option<DaemonHandshake>> {
    let path = paths.daemon_handshake();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    Ok(match serde_json::from_str(&text) {
        Ok(handshake) => Some(handshake),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "daemon handshake file is unreadable");
            None
        }
    })
}

/// Where to connect, honouring the environment overrides.
///
/// `GINKA_DAEMON_ADDRESS` and `GINKA_DAEMON_TOKEN` point a client at a daemon
/// it did not spawn — a debug daemon, one under a test harness, one being
/// stepped through in a debugger.
pub fn discover(paths: &Paths) -> Result<Option<(String, String)>> {
    let address = std::env::var(DAEMON_ADDRESS_ENV).ok();
    let token = std::env::var(DAEMON_TOKEN_ENV).ok();
    if let (Some(address), Some(token)) = (address, token) {
        return Ok(Some((format!("ws://{address}/rpc"), token)));
    }
    Ok(read(paths)?.map(|handshake| (handshake.endpoint(), handshake.token)))
}

/// Remove the advertisement. Idempotent: a daemon that never published, or
/// one whose file was already cleaned up, still shuts down cleanly.
pub fn withdraw(paths: &Paths) -> Result<()> {
    let path = paths.daemon_handshake();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

/// Compare a presented token against the expected one without leaking, in
/// timing, how much of it was right.
///
/// An empty token never matches: an unset environment variable and a daemon
/// that failed to generate one must not authenticate each other.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    if expected.is_empty() || expected.len() != presented.len() {
        return false;
    }
    expected
        .bytes()
        .zip(presented.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// 128 random bits, hex encoded, from the same generator UUIDs use.
fn generate_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn new_epoch() -> u64 {
    chrono::Utc::now().timestamp_micros().max(0) as u64
}

fn write(paths: &Paths, handshake: &DaemonHandshake) -> Result<()> {
    let path = paths.daemon_handshake();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_string_pretty(handshake)?)
        .with_context(|| format!("writing {}", temp.display()))?;

    // Tightened before the rename, so the token is never momentarily readable
    // by anyone else under its final name.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", temp.display()))?;
    }

    std::fs::rename(&temp, &path).with_context(|| format!("publishing {}", path.display()))?;
    Ok(())
}

/// Decide whether a connection may proceed.
///
/// Version first, then token: a client on the wrong contract should be told
/// that rather than sent chasing a token problem it does not have. Nothing at
/// all is served before the hello — a connection that has not identified
/// itself has no business asking questions.
pub fn greet(expected_token: &str, epoch: u64, first: &ClientMessage) -> ServerMessage {
    let ClientMessage::Hello {
        protocol_version,
        token,
    } = first
    else {
        return ServerMessage::Rejected {
            reason: HandshakeRejection::HelloExpected,
        };
    };

    if *protocol_version != PROTOCOL_VERSION {
        return ServerMessage::Rejected {
            reason: HandshakeRejection::VersionMismatch {
                daemon: PROTOCOL_VERSION,
            },
        };
    }
    if !token_matches(expected_token, token) {
        return ServerMessage::Rejected {
            reason: HandshakeRejection::BadToken,
        };
    }

    ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        epoch,
    }
}
