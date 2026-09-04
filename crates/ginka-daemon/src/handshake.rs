//! The discovery file: `~/.ginka/daemon.json`.
//!
//! It is how the app and the CLI find a running daemon, and it is the only
//! place the bearer token exists on disk. Written `0600`, replaced atomically,
//! and removed on a clean exit.
//!
//! A daemon that was killed leaves its file behind. Nothing here probes
//! whether the pid is alive: the connection attempt is the probe, and a client
//! that cannot connect treats the file as stale and starts a daemon of its own.

use anyhow::{Context, Result};
use ginka_protocol::Handshake;
use std::path::Path;

/// Write the discovery file for a daemon that is now listening.
///
/// The file is written to a temporary name and renamed, so a client reading it
/// concurrently sees either the old daemon's details or the new one's, never a
/// truncated file.
pub fn write(path: &Path, handshake: &Handshake) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_string_pretty(handshake)?)
        .with_context(|| format!("writing {}", temp.display()))?;
    restrict(&temp)?;
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Read the discovery file, if there is one.
pub fn read(path: &Path) -> Option<Handshake> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the discovery file. Missing is success: a daemon that never wrote
/// one still has to be able to shut down cleanly.
pub fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

/// Restrict a file to its owner.
///
/// The bearer token is in it, and `~/.ginka` is not necessarily private on a
/// shared machine. On platforms without Unix permissions this is a no-op and
/// the loopback bind plus the token are what stand between a local process and
/// the daemon.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(port: u16) -> Handshake {
        Handshake {
            protocol_version: ginka_protocol::envelope::PROTOCOL_VERSION,
            port,
            token: "secret".into(),
            pid: std::process::id(),
            version: "0.0.0".into(),
            epoch: 1,
        }
    }

    #[test]
    fn a_handshake_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        write(&path, &sample(8080)).unwrap();
        assert_eq!(read(&path).unwrap().port, 8080);
    }

    #[test]
    fn the_token_is_not_readable_by_other_users() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        write(&path, &sample(1)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the bearer token is in this file");
        }
    }

    #[test]
    fn rewriting_replaces_the_previous_daemons_details() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        write(&path, &sample(1)).unwrap();
        write(&path, &sample(2)).unwrap();
        assert_eq!(read(&path).unwrap().port, 2);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        assert!(read(&path).is_none());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read(&path).is_none());
    }

    #[test]
    fn removing_a_file_that_is_not_there_is_success() {
        let dir = tempfile::tempdir().unwrap();
        remove(&dir.path().join("absent.json")).unwrap();
    }
}
