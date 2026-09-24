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
    ginka_core::crash::install(paths.logs(), "daemon");

    // One daemon per state directory. Taken before anything else — before
    // the migrations, which are what makes a first start slow — so a second
    // one started in that window finds it held and leaves quietly.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.daemon_lock())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            tracing::info!("another daemon holds this state directory; leaving it to that one");
            return Ok(());
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }

    let config: settings::DaemonSettings = settings::load(&paths.daemon_settings());
    let daemon = Daemon::bind(paths, config)?;
    let handshake = daemon.handshake();
    tracing::info!(port = handshake.port, pid = handshake.pid, "daemon ready");
    // Print the port on stdout so a parent that spawned us can wait for it
    // without polling the handshake file.
    println!("listening on 127.0.0.1:{}", handshake.port);

    let served = smol::block_on(daemon.serve());
    // Held for the daemon's whole life; the OS releases it if the process
    // dies, which is what makes a crashed daemon replaceable.
    drop(lock);
    served
}
