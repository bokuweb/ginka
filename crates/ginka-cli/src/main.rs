//! The `ginka` command.
//!
//! Everything the UI can do, this can do, because both speak the same daemon
//! protocol (`AGENTS.md` rule 3). Until the daemon's RPC lands in M2 these
//! subcommands call `ginka-core` directly; the surface is the same either way,
//! so moving them behind the protocol is a change of transport, not of shape.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ginka_core::{Paths, db, git, project, registry, settings};
use rusqlite::Connection;
use std::path::PathBuf;

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
    /// Manage registered projects.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Manage workspaces, which are git worktrees.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Register a repository or folder.
    Add {
        /// Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// List registered projects.
    List,
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// List workspaces, reconciling against git first.
    List {
        /// Limit to one project.
        project: Option<String>,
    },
    /// Create a worktree on a new branch.
    New {
        project: String,
        /// The branch to create. Also becomes the workspace's immutable name.
        branch: String,
        /// What to branch from. Defaults to the project's default branch.
        #[arg(long)]
        base: Option<String>,
    },
    /// Remove a workspace's worktree.
    Remove {
        project: String,
        /// The workspace's immutable name, as shown by `workspace list`.
        name: String,
        /// Remove even when the worktree has uncommitted changes.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    paths.ensure()?;

    match cli.command {
        Command::Doctor => doctor(&paths),
        Command::Project(command) => {
            let conn = db::open(&paths.database())?;
            match command {
                ProjectCommand::Add { path } => project_add(&conn, path),
                ProjectCommand::List => project_list(&conn),
            }
        }
        Command::Workspace(command) => {
            let conn = db::open(&paths.database())?;
            match command {
                WorkspaceCommand::List { project } => workspace_list(&conn, project),
                WorkspaceCommand::New {
                    project,
                    branch,
                    base,
                } => workspace_new(&paths, &conn, &project, &branch, base),
                WorkspaceCommand::Remove {
                    project,
                    name,
                    force,
                } => workspace_remove(&conn, &project, &name, force),
            }
        }
    }
}

fn doctor(paths: &Paths) -> Result<()> {
    println!("root          {}", paths.root().display());
    println!("database      {}", paths.database().display());

    let conn = db::open(&paths.database())?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    println!("schema        v{version}");

    let app: settings::AppSettings = settings::load(&paths.app_settings());
    println!("appearance    {:?}", app.appearance);
    println!("projects      {}", project::list_projects(&conn)?.len());
    println!(
        "daemon        {}",
        if paths.daemon_handshake().exists() {
            "handshake present"
        } else {
            "not running"
        }
    );
    Ok(())
}

fn project_add(conn: &Connection, path: Option<PathBuf>) -> Result<()> {
    let path = path.unwrap_or(std::env::current_dir()?);
    let project = registry::register_project(conn, &path)?;
    let report = registry::sync_worktrees(conn, &project)?;
    println!(
        "registered {} ({:?}) at {}",
        project.name,
        project.kind,
        project.path.display()
    );
    if report.added > 0 {
        println!("adopted {} worktree(s)", report.added);
    }
    Ok(())
}

fn project_list(conn: &Connection) -> Result<()> {
    let projects = project::list_projects(conn)?;
    if projects.is_empty() {
        println!("no projects registered; try `ginka project add <path>`");
        return Ok(());
    }
    for project in projects {
        println!(
            "{:<24} {:<6} {}",
            project.name.0,
            format!("{:?}", project.kind).to_lowercase(),
            project.path.display()
        );
    }
    Ok(())
}

/// Resolve a project by name, with a message that says what is available
/// rather than only that the name was wrong.
fn find_project(conn: &Connection, name: &str) -> Result<project::Project> {
    let projects = project::list_projects(conn)?;
    projects
        .iter()
        .find(|project| project.name.0 == name)
        .cloned()
        .with_context(|| {
            let known: Vec<&str> = projects.iter().map(|p| p.name.0.as_str()).collect();
            if known.is_empty() {
                format!("no project named {name}; none are registered")
            } else {
                format!("no project named {name}; registered: {}", known.join(", "))
            }
        })
}

fn workspace_list(conn: &Connection, only: Option<String>) -> Result<()> {
    let projects = match only {
        Some(name) => vec![find_project(conn, &name)?],
        None => project::list_projects(conn)?,
    };

    for project in projects {
        registry::sync_worktrees(conn, &project)?;
        for worktree in project::list_worktrees(conn, &project.name)? {
            println!(
                "{:<32} {:<24} {}",
                worktree.workspace_id().0,
                worktree.branch,
                worktree.path.display()
            );
        }
    }
    Ok(())
}

fn workspace_new(
    paths: &Paths,
    conn: &Connection,
    project: &str,
    branch: &str,
    base: Option<String>,
) -> Result<()> {
    let project = find_project(conn, project)?;
    let base = base.unwrap_or_else(|| project.default_branch.clone());

    // Worktrees live under Ginka's own directory rather than beside the user's
    // checkout, so the app never litters the repository it was pointed at.
    let path = paths
        .worktrees()
        .join(&project.name.0)
        .join(ginka_protocol::ids::slugify(branch));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    git::add_worktree(&project.path, &path, branch, &base)?;
    registry::sync_worktrees(conn, &project)?;
    println!("created {} at {}", branch, path.display());
    Ok(())
}

fn workspace_remove(conn: &Connection, project: &str, name: &str, force: bool) -> Result<()> {
    let project = find_project(conn, project)?;
    let worktree = project::list_worktrees(conn, &project.name)?
        .into_iter()
        .find(|worktree| worktree.name == name)
        .with_context(|| format!("no workspace named {name} in {}", project.name))?;

    git::remove_worktree(&project.path, &worktree.path, force)?;
    registry::sync_worktrees(conn, &project)?;
    println!("removed {}", worktree.workspace_id().0);
    Ok(())
}
