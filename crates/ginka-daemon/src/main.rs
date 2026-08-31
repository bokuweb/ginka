//! The Ginka daemon.
//!
//! Owns SQLite, agent processes, PTYs and git, and outlives the UI so agents
//! keep running when the window closes. The RPC server lands in M2; today this
//! binary proves the storage and settings layers boot headless.

use anyhow::Result;
use ginka_core::{Paths, db, logging, settings};

fn main() -> Result<()> {
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let _log_guard = logging::init(&paths, "daemon")?;

    let config: settings::DaemonSettings = settings::load(&paths.daemon_settings());
    let _conn = db::open(&paths.database())?;

    tracing::info!(
        root = %paths.root().display(),
        sync_interval_secs = config.sync_interval_secs,
        "daemon storage ready (RPC server lands in M2)"
    );
    Ok(())
}
