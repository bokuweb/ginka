//! The `ginka` command.
//!
//! Everything the UI can do, this can do, because both speak the same daemon
//! protocol (`AGENTS.md` rule 3). The subcommands land alongside the RPC
//! surface in M4; `doctor` exists now because a scaffold you cannot verify is
//! a scaffold you do not trust.

use anyhow::Result;
use clap::{Parser, Subcommand};
use ginka_core::{Paths, db, settings};

#[derive(Parser)]
#[command(
    name = "ginka",
    version,
    about = "IDE-agnostic coding-agent orchestrator"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report where Ginka keeps its state and whether that state is healthy.
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => doctor(),
    }
}

fn doctor() -> Result<()> {
    let paths = Paths::from_env()?;
    println!("root          {}", paths.root().display());
    println!("database      {}", paths.database().display());

    paths.ensure()?;
    let conn = db::open(&paths.database())?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    println!("schema        v{version}");

    let app: settings::AppSettings = settings::load(&paths.app_settings());
    println!("appearance    {:?}", app.appearance);

    let handshake = paths.daemon_handshake();
    println!(
        "daemon        {}",
        if handshake.exists() {
            "handshake present"
        } else {
            "not running"
        }
    );
    Ok(())
}
