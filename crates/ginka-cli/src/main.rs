//! The `ginka` command.
//!
//! Everything the UI can do, this can do, because both speak the same daemon
//! protocol (`AGENTS.md` rule 3). Every subcommand here is one `Request`: the
//! CLI holds no state, opens no database and runs no git — it finds a daemon,
//! starting one if there is none, and asks.
//!
//! `--json` prints the protocol's own response instead of a table, which is
//! what makes the command line usable by an agent as well as by a person.

// The CLI prints to a human too, so its own messages are translated. The
// protocol's shapes, printed by `--json`, are not: those are for programs.
rust_i18n::i18n!("../../locales", fallback = "en");

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ginka_client::{Client, Discovery};
use ginka_core::{Paths, project, settings};
use ginka_protocol::model::{
    AgentStatus, ChangeSource, Changes, Checkpoint, Project, Session, SessionMatch, UsageRow,
    WorkspaceSummary,
};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{CheckpointId, ProjectName, SessionId, WorkspaceId};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ginka",
    version,
    about = "IDE-agnostic coding-agent orchestrator"
)]
struct Cli {
    /// Print the daemon's own response as JSON instead of a table.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report where Ginka keeps its state and whether that state is healthy.
    Doctor,
    /// Inspect and control the background daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Manage registered projects.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Manage workspaces, which are git worktrees.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Report which agent CLIs this machine has, and whether they are usable.
    Agents,
    /// Start and steer agent sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// List a workspace's files, best matches first.
    Files {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// What to look for. Any subsequence of a path will do.
        query: Option<String>,
    },
    /// Report what the work has cost.
    Usage {
        /// How many days back to look.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Show what has changed in a workspace.
    Changes {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Show what is staged for the next commit instead of everything.
        #[arg(long)]
        staged: bool,
        /// Show what has happened since a checkpoint, by its id.
        #[arg(long)]
        since: Option<String>,
        /// Print the diff itself rather than a summary.
        #[arg(long)]
        patch: bool,
    },
    /// Commit a workspace's work.
    Commit {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The commit message.
        message: String,
        /// Commit only what is already staged.
        #[arg(long)]
        staged: bool,
    },
    /// Push a workspace's branch, setting an upstream if it has none.
    Push {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// Rewind a workspace to a saved state.
    #[command(subcommand)]
    Checkpoint(CheckpointCommand),
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Report whether a daemon is running, and where.
    Status,
    /// Start a daemon if there is not one already.
    Start,
    /// Ask the running daemon to exit. Agents it is running are stopped.
    Stop,
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
    /// Forget a project. Its files are left alone.
    Remove { project: String },
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
    /// Make somewhere to work with no project at all.
    ///
    /// Creates a dated directory under Ginka's own state and registers it, so
    /// a question that needs a scratch folder does not need a repository.
    Scratch {
        /// What to call it. Defaults to `scratch`.
        name: Option<String>,
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
    /// Pin a workspace so it sorts first.
    Pin {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Unpin instead.
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// List sessions, most recently active first.
    List {
        /// Limit to one workspace, by id.
        workspace: Option<String>,
    },
    /// Start an agent in a workspace.
    Start {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The opening prompt.
        prompt: String,
        /// Which agent to run.
        #[arg(long, default_value = "claude")]
        agent: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Send a follow-up. Queued if the agent is still working.
    Send { session: String, text: String },
    /// Stop an agent's process tree.
    Cancel { session: String },
    /// Rename a conversation.
    Rename { session: String, title: String },
    /// Forget a session, its transcript and its checkpoints.
    Remove { session: String },
    /// Take a copy of a conversation as it was, and carry on from there.
    Fork {
        session: String,
        /// The transcript position to fork at. Defaults to all of it.
        #[arg(long)]
        after: Option<u64>,
    },
    /// Find what was said, across conversations.
    Search {
        query: String,
        /// Limit to one workspace, by id.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Print a session's transcript.
    Log {
        session: String,
        /// Start after this transcript position.
        #[arg(long)]
        after: Option<u64>,
    },
}

#[derive(Subcommand)]
enum CheckpointCommand {
    /// List a workspace's checkpoints, newest first.
    List { workspace: String },
    /// Put a workspace back to a checkpoint's state.
    ///
    /// What is there now is snapshotted first, so this is reversible.
    Restore { checkpoint: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    paths.ensure()?;
    let app: settings::AppSettings = settings::load(&paths.app_settings());
    ginka_core::i18n::init(app.locale.as_deref());

    match cli.command {
        Command::Doctor => doctor(&paths),
        Command::Daemon(command) => daemon(&paths, command, cli.json),
        command => {
            // Read before the command is consumed: how a diff is printed is
            // the one thing the response alone does not say.
            let patch = matches!(command, Command::Changes { patch: true, .. });
            let request = request_for(command)?;
            let response = smol::block_on(async {
                let client = connect(&paths).await?;
                client
                    .request(request)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))
            })?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                print(response, patch);
            }
            Ok(())
        }
    }
}

/// Find the daemon, starting one if there is none.
async fn connect(paths: &Paths) -> Result<Client> {
    Discovery::new(paths.daemon_handshake())
        .with_home(paths.root())
        .connect(None)
        .await
        .context("could not reach the daemon")
}

/// Turn a subcommand into the one request that carries it out.
fn request_for(command: Command) -> Result<Request> {
    Ok(match command {
        Command::Project(ProjectCommand::Add { path }) => Request::AddProject {
            path: match path {
                Some(path) => path,
                None => std::env::current_dir()?,
            },
        },
        Command::Project(ProjectCommand::List) => Request::ListProjects,
        Command::Project(ProjectCommand::Remove { project }) => Request::RemoveProject {
            project: ProjectName(project),
        },

        Command::Workspace(WorkspaceCommand::List { project }) => Request::ListWorkspaces {
            project: project.map(ProjectName),
        },
        Command::Workspace(WorkspaceCommand::New {
            project,
            branch,
            base,
        }) => Request::CreateWorkspace {
            project: ProjectName(project),
            branch,
            base,
        },
        Command::Workspace(WorkspaceCommand::Scratch { name }) => {
            Request::CreateScratchWorkspace { name }
        }
        Command::Workspace(WorkspaceCommand::Remove {
            project,
            name,
            force,
        }) => Request::RemoveWorkspace {
            workspace: WorkspaceId::new(&ProjectName(project), &name),
            force,
        },
        Command::Workspace(WorkspaceCommand::Pin { workspace, off }) => Request::PinWorkspace {
            workspace: WorkspaceId(workspace),
            pinned: !off,
        },

        Command::Agents => Request::ListAgents,
        Command::Usage { days } => Request::Usage { days: Some(days) },
        Command::Files { workspace, query } => Request::WorkspaceFiles {
            workspace: WorkspaceId(workspace),
            query,
            limit: None,
        },
        Command::Commit {
            workspace,
            message,
            staged,
        } => Request::Commit {
            workspace: WorkspaceId(workspace),
            message,
            all: !staged,
        },
        Command::Push { workspace } => Request::Push {
            workspace: WorkspaceId(workspace),
        },
        Command::Changes {
            workspace,
            staged,
            since,
            ..
        } => Request::WorkspaceChanges {
            workspace: WorkspaceId(workspace),
            source: match (staged, since) {
                (_, Some(checkpoint)) => ChangeSource::SinceCheckpoint {
                    checkpoint: CheckpointId(checkpoint),
                },
                (true, None) => ChangeSource::Staged,
                (false, None) => ChangeSource::Uncommitted,
            },
        },
        Command::Session(SessionCommand::List { workspace }) => Request::ListSessions {
            workspace: workspace.map(WorkspaceId),
        },
        Command::Session(SessionCommand::Start {
            workspace,
            prompt,
            agent,
            model,
        }) => Request::StartSession {
            workspace: WorkspaceId(workspace),
            agent,
            prompt,
            model,
        },
        Command::Session(SessionCommand::Send { session, text }) => Request::SendMessage {
            session: SessionId(session),
            text,
        },
        Command::Session(SessionCommand::Cancel { session }) => Request::CancelSession {
            session: SessionId(session),
        },
        Command::Session(SessionCommand::Rename { session, title }) => Request::RenameSession {
            session: SessionId(session),
            title,
        },
        Command::Session(SessionCommand::Remove { session }) => Request::RemoveSession {
            session: SessionId(session),
        },
        Command::Session(SessionCommand::Fork { session, after }) => Request::ForkSession {
            session: SessionId(session),
            after,
        },
        Command::Session(SessionCommand::Search { query, workspace }) => Request::SearchSessions {
            workspace: workspace.map(WorkspaceId),
            query,
            limit: None,
        },
        Command::Session(SessionCommand::Log { session, after }) => Request::SessionTranscript {
            session: SessionId(session),
            after,
            limit: None,
        },

        Command::Checkpoint(CheckpointCommand::List { workspace }) => Request::ListCheckpoints {
            workspace: WorkspaceId(workspace),
        },
        Command::Checkpoint(CheckpointCommand::Restore { checkpoint }) => {
            Request::RestoreCheckpoint {
                checkpoint: CheckpointId(checkpoint),
            }
        }

        // Handled before this point, without a daemon.
        Command::Doctor | Command::Daemon(_) => unreachable!("handled in main"),
    })
}

/// Where state lives and whether it is healthy.
///
/// This is the command someone runs when something is wrong, so it reads the
/// state directly rather than through the daemon: a daemon that will not start
/// is exactly what it has to be able to report.
fn doctor(paths: &Paths) -> Result<()> {
    println!("root          {}", paths.root().display());
    println!("database      {}", paths.database().display());

    let conn = ginka_core::db::open(&paths.database())?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    println!("schema        v{version}");

    let app: settings::AppSettings = settings::load(&paths.app_settings());
    println!("appearance    {:?}", app.appearance);
    println!("projects      {}", project::list_projects(&conn)?.len());
    println!("daemon        {}", describe_daemon(paths));
    Ok(())
}

fn daemon(paths: &Paths, command: DaemonCommand, json: bool) -> Result<()> {
    let discovery = Discovery::new(paths.daemon_handshake()).with_home(paths.root());
    match command {
        DaemonCommand::Status => {
            if json {
                println!("{}", serde_json::to_string_pretty(&discovery.published())?);
            } else {
                println!("{}", describe_daemon(paths));
            }
            Ok(())
        }
        DaemonCommand::Start => {
            let client = smol::block_on(discovery.connect(None))?;
            println!(
                "{}",
                rust_i18n::t!("cli.daemon.listening", version = client.daemon_version())
            );
            Ok(())
        }
        DaemonCommand::Stop => smol::block_on(async {
            match discovery.connect_existing(None).await {
                Ok(client) => {
                    match client.request(Request::Shutdown).await {
                        Ok(_) => {}
                        // The daemon going is the thing that was asked for, and
                        // it may go before its answer is read. What would be a
                        // failure for any other request is the outcome here.
                        Err(error) if error.code == "failed" => {
                            tracing_stop_noted(&error.message);
                        }
                        Err(error) => return Err(anyhow::anyhow!("{error}")),
                    }
                    println!("{}", rust_i18n::t!("cli.daemon.stopping"));
                    Ok(())
                }
                // Nothing to stop is the state the user asked for.
                Err(_) => {
                    println!("{}", rust_i18n::t!("cli.daemon.none"));
                    Ok(())
                }
            }
        }),
    }
}

/// Note a connection that ended while the daemon was stopping.
///
/// Not an error: the daemon is meant to be gone, and whether its last frame
/// arrived first is not something the user asked about.
fn tracing_stop_noted(why: &str) {
    tracing::debug!(why, "the daemon went before answering");
}

fn describe_daemon(paths: &Paths) -> String {
    match Discovery::new(paths.daemon_handshake()).published() {
        Some(handshake) => rust_i18n::t!(
            "cli.daemon.running",
            pid = handshake.pid,
            port = handshake.port,
            version = handshake.version
        )
        .to_string(),
        None => rust_i18n::t!("cli.daemon.stopped").to_string(),
    }
}

/// Print a response as a person reads it.
fn print(response: Response, patch: bool) {
    match response {
        Response::Ack => println!("{}", rust_i18n::t!("cli.done")),
        Response::Projects { projects } => print_projects(&projects),
        Response::Project { project } => print_projects(std::slice::from_ref(&project)),
        Response::Workspaces { workspaces } => print_workspaces(&workspaces),
        Response::Workspace { workspace } => print_workspaces(std::slice::from_ref(&workspace)),
        Response::Agents { agents } => print_agents(&agents),
        Response::Sessions { sessions } => print_sessions(&sessions),
        Response::SessionMatches { matches } => print_matches(&matches),
        Response::Session { session } => print_sessions(std::slice::from_ref(&session)),
        Response::Checkpoints { checkpoints } => print_checkpoints(&checkpoints),
        Response::Changes { changes } => print_changes(&changes, patch),
        Response::Files { files } => {
            for file in files {
                println!("{}", file.path);
            }
        }
        Response::Draft { text } => println!("{text}"),
        Response::Usage { by_day, by_agent } => print_usage(&by_day, &by_agent),
        Response::Committed { commit } => println!(
            "{}",
            rust_i18n::t!("cli.committed", commit = &commit[..commit.len().min(12)])
        ),
        Response::Transcript { entries } => {
            if entries.is_empty() {
                println!("{}", rust_i18n::t!("cli.transcript.empty"));
            }
            for entry in entries {
                println!("{}", ginka_cli_format::transcript_line(&entry));
            }
        }
    }
}

fn print_projects(projects: &[Project]) {
    if projects.is_empty() {
        println!("{}", rust_i18n::t!("cli.projects.empty"));
        return;
    }
    for project in projects {
        println!(
            "{:<24} {:<6} {}",
            project.name.0,
            project.kind.as_str(),
            project.path.display()
        );
    }
}

fn print_workspaces(workspaces: &[WorkspaceSummary]) {
    if workspaces.is_empty() {
        println!("{}", rust_i18n::t!("cli.workspaces.empty"));
        return;
    }
    for summary in workspaces {
        println!(
            "{:<32} {:<24} {:<10} {}",
            summary.id().0,
            summary.worktree.branch,
            ginka_cli_format::status_label(&summary.status),
            summary.worktree.path.display()
        );
    }
}

/// What each agent CLI says about itself.
///
/// The readiness column is the point: an agent that is missing or signed out
/// should be visible here rather than in a session that failed.
fn print_agents(agents: &[AgentStatus]) {
    for agent in agents {
        let state = if !agent.installed {
            rust_i18n::t!("cli.agent.missing").to_string()
        } else if agent.authenticated == Some(false) {
            rust_i18n::t!("cli.agent.signed_out").to_string()
        } else {
            rust_i18n::t!("cli.agent.ready").to_string()
        };
        println!(
            "{:<10} {:<14} {:<12} {:<14} {}",
            agent.id,
            agent.display_name,
            agent.version.clone().unwrap_or_default(),
            state,
            agent.detail.clone().unwrap_or_default()
        );
    }
}

fn print_sessions(sessions: &[Session]) {
    if sessions.is_empty() {
        println!("{}", rust_i18n::t!("cli.sessions.empty"));
        return;
    }
    for session in sessions {
        println!(
            "{:<34} {:<24} {:<10} {:<8} {}",
            session.id.0,
            session.workspace.0,
            session.agent,
            session.state.as_str(),
            // The title says what the conversation is about; the summary says
            // what it is doing. A list is read for the first.
            session
                .title
                .clone()
                .or_else(|| session.summary.clone())
                .unwrap_or_default()
        );
    }
}

/// What changed, as a summary or as the diff itself.
fn print_changes(changes: &Changes, patch: bool) {
    if changes.is_empty() {
        println!("{}", rust_i18n::t!("cli.changes.empty"));
        return;
    }
    for file in &changes.files {
        println!(
            "{:<4} +{:<5} -{:<5} {}",
            match file.kind {
                ginka_protocol::ChangeKind::Added => "add",
                ginka_protocol::ChangeKind::Modified => "mod",
                ginka_protocol::ChangeKind::Deleted => "del",
                ginka_protocol::ChangeKind::Renamed => "ren",
            },
            file.added,
            file.removed,
            file.label()
        );
        if !patch {
            continue;
        }
        for hunk in &file.hunks {
            println!("  {}", hunk.header);
            for line in &hunk.lines {
                let marker = match line.kind {
                    ginka_protocol::LineKind::Added => '+',
                    ginka_protocol::LineKind::Removed => '-',
                    ginka_protocol::LineKind::Context => ' ',
                };
                println!("  {marker}{}", line.text);
            }
        }
    }
    let (added, removed) = changes.totals();
    println!(
        "{}",
        rust_i18n::t!(
            "cli.changes.total",
            files = changes.files.len(),
            added = added,
            removed = removed
        )
    );
}

/// Where a search found what it was looking for.
fn print_matches(matches: &[SessionMatch]) {
    if matches.is_empty() {
        println!("{}", rust_i18n::t!("cli.search.empty"));
        return;
    }
    for found in matches {
        println!(
            "{:<34} {:<5} {:<24} {}",
            found.session.0,
            found.seq,
            found.title.clone().unwrap_or_default(),
            found.excerpt
        );
    }
}

/// What the work cost, by day and by agent.
fn print_usage(by_day: &[UsageRow], by_agent: &[UsageRow]) {
    if by_day.is_empty() {
        println!("{}", rust_i18n::t!("cli.usage.empty"));
        return;
    }
    let row = |row: &UsageRow| {
        println!(
            "{:<14} {:>10} in {:>8} out {:>8} cached  {:>4} turns  {}",
            row.label,
            row.totals.input_tokens,
            row.totals.output_tokens,
            row.totals.cache_read_tokens,
            row.totals.turns,
            // A vendor that does not price its work says nothing rather than
            // zero, which would be a claim that it was free.
            row.totals
                .cost_usd
                .map(|cost| format!("${cost:.2}"))
                .unwrap_or_default()
        )
    };
    for entry in by_day {
        row(entry);
    }
    println!();
    for entry in by_agent {
        row(entry);
    }
}

fn print_checkpoints(checkpoints: &[Checkpoint]) {
    if checkpoints.is_empty() {
        println!("{}", rust_i18n::t!("cli.checkpoints.empty"));
        return;
    }
    for checkpoint in checkpoints {
        println!(
            "{:<34} turn {:<4} {:<12} {}",
            checkpoint.id.0,
            checkpoint.turn,
            &checkpoint.commit[..checkpoint.commit.len().min(12)],
            checkpoint.label
        );
    }
}

/// Formatting shared by the printers, kept apart so it can be tested.
mod ginka_cli_format {
    use ginka_protocol::AgentEvent;
    use ginka_protocol::model::{BranchStatus, TranscriptEntry, TranscriptPayload};

    /// A one-word summary of a worktree's git status.
    pub fn status_label(status: &BranchStatus) -> String {
        if status.conflict {
            return "conflict".to_string();
        }
        let mut parts = Vec::new();
        if status.dirty {
            parts.push("dirty".to_string());
        }
        if status.ahead > 0 {
            parts.push(format!("+{}", status.ahead));
        }
        if status.behind > 0 {
            parts.push(format!("-{}", status.behind));
        }
        if parts.is_empty() {
            return "clean".to_string();
        }
        parts.join(" ")
    }

    /// One transcript entry as a line of output.
    pub fn transcript_line(entry: &TranscriptEntry) -> String {
        match &entry.payload {
            TranscriptPayload::User { text } => format!("{:>4}  you  {text}", entry.seq),
            TranscriptPayload::Agent { event } => {
                format!("{:>4}  {}", entry.seq, describe(event))
            }
        }
    }

    fn describe(event: &AgentEvent) -> String {
        match event {
            AgentEvent::TextDelta { text } => text.clone(),
            AgentEvent::Reasoning { text } => format!("(thinking) {text}"),
            AgentEvent::ToolCall { name, input, .. } => format!("[{name}] {input}"),
            AgentEvent::ToolResult {
                output, is_error, ..
            } => {
                let marker = if *is_error { "!" } else { " " };
                format!("[result]{marker} {}", output.lines().next().unwrap_or(""))
            }
            AgentEvent::AskUser { question, .. } => format!("? {question}"),
            AgentEvent::PlanProposal { plan, .. } => format!("plan: {plan}"),
            AgentEvent::Usage { usage } => format!(
                "usage: {} in, {} out",
                usage.input_tokens, usage.output_tokens
            ),
            AgentEvent::TurnEnd { turn } => format!("-- end of turn {turn} --"),
            AgentEvent::SessionResult { state, summary } => format!(
                "== {} {}",
                state.as_str(),
                summary.clone().unwrap_or_default()
            ),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_clean_worktree_says_so_rather_than_showing_nothing() {
            assert_eq!(status_label(&BranchStatus::default()), "clean");
        }

        #[test]
        fn a_conflict_outranks_everything_else_worth_saying() {
            let status = BranchStatus {
                conflict: true,
                dirty: true,
                ahead: 2,
                ..BranchStatus::default()
            };
            assert_eq!(status_label(&status), "conflict");
        }

        #[test]
        fn divergence_is_shown_next_to_dirtiness() {
            let status = BranchStatus {
                dirty: true,
                ahead: 2,
                behind: 1,
                ..BranchStatus::default()
            };
            assert_eq!(status_label(&status), "dirty +2 -1");
        }

        #[test]
        fn a_transcript_line_says_who_spoke() {
            let entry = TranscriptEntry {
                seq: 7,
                at: 0,
                payload: TranscriptPayload::User {
                    text: "write the test first".into(),
                },
            };
            assert!(transcript_line(&entry).contains("you  write the test first"));
        }

        #[test]
        fn a_failing_tool_result_is_marked_in_the_log() {
            let entry = TranscriptEntry {
                seq: 1,
                at: 0,
                payload: TranscriptPayload::Agent {
                    event: AgentEvent::ToolResult {
                        id: "t".into(),
                        output: "no such file\nmore".into(),
                        is_error: true,
                    },
                },
            };
            let line = transcript_line(&entry);
            assert!(line.contains("[result]!"), "{line}");
            assert!(!line.contains("more"), "only the first line: {line}");
        }
    }
}
