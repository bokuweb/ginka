//! The `ginka-daemon` binary.
//!
//! One daemon per user, started by whichever of the app or the CLI notices
//! there is none. It outlives them both: agents keep running when the window
//! closes, which is the whole reason the process boundary exists.

use anyhow::Result;
use ginka_core::{Paths, logging, settings};
use ginka_daemon::Daemon;

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let _log_guard = logging::init(&paths, "daemon")?;

    let config: settings::DaemonSettings = settings::load(&paths.daemon_settings());
    let daemon = Daemon::bind(paths, config)?;
    let handshake = daemon.handshake();
    tracing::info!(port = handshake.port, pid = handshake.pid, "daemon ready");
    // Print the port on stdout so a parent that spawned us can wait for it
    // without polling the handshake file.
    println!("listening on 127.0.0.1:{}", handshake.port);

    smol::block_on(daemon.serve())
}
