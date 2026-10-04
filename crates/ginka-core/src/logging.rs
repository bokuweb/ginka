//! Tracing setup shared by the daemon, the CLI and the app.

use crate::Paths;
use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Install tracing: a daily-rotated file in `~/.ginka/logs/` plus stderr.
///
/// The returned guard flushes the file writer on drop; hold it for the life of
/// the process or log lines are lost at exit.
pub fn init(paths: &Paths, process: &str) -> Result<WorkerGuard> {
    std::fs::create_dir_all(paths.logs())?;
    let file = tracing_appender::rolling::daily(paths.logs(), format!("{process}.log"));
    let (file, guard) = tracing_appender::non_blocking(file);

    let filter =
        EnvFilter::try_from_env("GINKA_LOG").unwrap_or_else(|_| EnvFilter::new("info,ginka=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(file).with_ansi(false))
        .with(fmt::layer().with_writer(std::io::stderr))
        .try_init()
        .ok();

    Ok(guard)
}
