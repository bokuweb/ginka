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
    Account, AgentStatus, ChangeSource, Changes, Checkpoint, ConnectorState, PlanSnapshot, Project,
    Session, SessionMatch, UsageRow, WorkspaceSummary,
};
use ginka_protocol::provider::ProviderKind;
use ginka_protocol::rpc::{Attempt, Request, Response};
use ginka_protocol::{AccountId, CheckpointId, ProjectName, SessionId, WorkspaceId};
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
    /// Manage logins: several per provider, each in a directory of its own.
    ///
    /// A session runs on one of them, and the usage report says what each
    /// one spent and how much of its rate-limit window is left.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Start and steer agent sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// Store a file the daemon keeps, and print the reference a message
    /// refers to it by.
    ///
    /// Mention the reference in a prompt and the agent is handed the file's
    /// path: `ginka session send <id> "review $(ginka attach diff.patch)"`.
    Attach {
        /// The file to store.
        path: PathBuf,
    },
    /// List a workspace's files, best matches first.
    Files {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// What to look for. Any subsequence of a path will do.
        query: Option<String>,
        /// Maximum number of paths to return.
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Ask the same question in several worktrees at once.
    ///
    /// One worktree per attempt, so the attempts cannot tread on each other,
    /// and the branches are `<prefix>-1`, `<prefix>-2`, …
    FanOut {
        /// The project to cut the worktrees in.
        project: String,
        /// What the branches are called.
        prefix: String,
        /// The prompt every attempt is given.
        prompt: String,
        /// One per attempt: a driver id, optionally `agent:model`. Repeats are
        /// how the same agent is asked twice.
        #[arg(long = "agent", required = true)]
        agents: Vec<String>,
        /// What to branch from. The project's default branch otherwise.
        #[arg(long)]
        base: Option<String>,
    },
    /// Find lines in a workspace's files.
    Search {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The text to look for. Taken literally, not as a pattern.
        query: String,
    },
    /// Print one of a workspace's files.
    Show {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
    },
    /// Save an existing UTF-8 workspace file without overwriting a newer edit.
    Save {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
        /// The revision printed by `--json show`; protects concurrent edits.
        #[arg(long)]
        expected_revision: String,
        /// The complete replacement text. Read from stdin when omitted.
        #[arg(long)]
        text: Option<String>,
    },
    /// The skills the agents can load, and whether each is on.
    #[command(subcommand)]
    Skills(SkillsCommand),
    /// List the commands a workspace offers after `/`.
    Commands {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// The Slack connector: a bound channel starts an agent here, and the
    /// answer goes back to the thread (`docs/connectors.md`).
    #[command(subcommand)]
    Slack(SlackCommand),
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
    /// Leave comments on a diff and send them back to the agent.
    #[command(subcommand)]
    Review(ReviewCommand),
    /// Put a file into the next commit, or take it back out.
    Stage {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
        /// Take it back out instead of putting it in.
        #[arg(long)]
        undo: bool,
    },
    /// Throw away a file's uncommitted work.
    ///
    /// A file the agent created is deleted, which git cannot undo.
    Revert {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
    },
    /// Commit a workspace's work.
    Commit {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The commit message. Omit it with `--generate`.
        message: Option<String>,
        /// Commit only what is already staged.
        #[arg(long)]
        staged: bool,
        /// Have an agent write the message on its cheap tier, print it, and
        /// commit with it.
        #[arg(long)]
        generate: bool,
        /// Which agent writes it: claude, codex. The workspace's latest
        /// session's agent otherwise.
        #[arg(long, requires = "generate")]
        agent: Option<String>,
    },
    /// Push a workspace's branch, setting an upstream if it has none.
    Push {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// Serve Ginka's operations to an agent over MCP, on stdin and stdout.
    ///
    /// Spawned by the agent, not by the user: the state stays in the daemon
    /// and this is a bridge to it.
    Mcp,
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
        /// Reader-facing name; the stable project key still follows the folder.
        #[arg(long)]
        label: Option<String>,
    },
    /// List registered projects.
    List,
    /// Find literal source lines across every active workspace.
    Search {
        project: String,
        query: String,
        /// Maximum hits across all workspaces.
        #[arg(long)]
        limit: Option<u32>,
    },
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
    /// List the repository's local branches, and where each is checked out.
    Branches {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// Check a branch out in a workspace. The workspace keeps its id.
    Checkout {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        branch: String,
        /// Create the branch from HEAD first.
        #[arg(long)]
        create: bool,
    },
    /// Build zvec-grep's index for a workspace, here in this terminal, so
    /// agents started in it get semantic search. Needs `zg` on PATH.
    Index {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// Pin a workspace so it sorts first.
    Pin {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Unpin instead.
        #[arg(long)]
        off: bool,
    },
    /// Archive a workspace without removing its worktree or conversation.
    Archive {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Restore it to the active project tree instead.
        #[arg(long)]
        restore: bool,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// List every login, with whether it is signed in and the latest reading
    /// of its rate-limit windows.
    List,
    /// Add a login for a provider.
    ///
    /// Makes a private directory for the provider's CLI to sign into; nothing
    /// is signed in until `account login`.
    Add {
        /// A slug, unique across providers: `claude-work`, `codex-personal`.
        id: String,
        /// Which provider's login this is: `claude`, `codex`.
        #[arg(long)]
        provider: String,
        /// What the account is called in the window. The id otherwise.
        #[arg(long)]
        label: Option<String>,
    },
    /// Forget a login. Its directory — the vendor's sign-in — is kept unless
    /// asked otherwise.
    Remove {
        id: String,
        /// Delete the directory too, sign-in and all.
        #[arg(long)]
        delete_home: bool,
    },
    /// Run the vendor's own sign-in for a login, here in this terminal.
    Login { id: String },
    /// Ask the provider how much of a login's rate-limit windows is left.
    Refresh { id: String },
}

#[derive(Subcommand)]
enum SlackCommand {
    /// Whether the connector is configured, connected, and listening where.
    Status,
    /// The channels the bot listens in, and where each one runs.
    Bindings,
    /// Let one more Slack member speak to the bot, by member id (`U…`).
    ///
    /// Written into `settings.json`; the running connector picks it up.
    Allow {
        /// The member id, from the person's Slack profile.
        sender: String,
    },
    /// Post one message into a channel and take it back, to prove the
    /// tokens and the channel id are right.
    Test {
        /// The conversation id (`C…`), not the name.
        channel: String,
    },
}

#[derive(Subcommand)]
enum SkillsCommand {
    /// List every skill, grouped across the places it was installed.
    List {
        /// Only this project's skills, plus the user's own.
        #[arg(long)]
        project: Option<String>,
    },
    /// Turn every copy of a skill on.
    Enable {
        name: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Hide a skill from every agent by renaming its SKILL.md. Nothing is
    /// deleted.
    Disable {
        name: String,
        #[arg(long)]
        project: Option<String>,
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
        /// Reasoning level advertised for the selected model.
        #[arg(long)]
        reasoning_effort: Option<String>,
        /// Service tier advertised for the selected model.
        #[arg(long)]
        service_tier: Option<String>,
        /// Which login to run on, by id. The provider's default otherwise.
        #[arg(long)]
        account: Option<String>,
        /// What the agent may touch: read-only, ask (edit freely, commands
        /// sandboxed or refused) or auto (edit and run). Defaults to ask.
        #[arg(long, value_parser = parse_access)]
        access: Option<ginka_protocol::AccessMode>,
    },
    /// Send a follow-up. Queued if the agent is still working.
    Send { session: String, text: String },
    /// Answer a question, plan or permission request in a running turn.
    Respond {
        session: String,
        /// The request id shown in the transcript.
        request_id: String,
        response: String,
    },
    /// Replace provider options for later turns. Omitted options use the
    /// provider default.
    Options {
        session: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        reasoning_effort: Option<String>,
        #[arg(long)]
        service_tier: Option<String>,
    },
    /// Stop an agent's process tree.
    Cancel { session: String },
    /// Rename a conversation.
    Rename { session: String, title: String },
    /// Forget a session, its transcript and its checkpoints.
    Remove { session: String },
    /// Take a copy of a conversation as it was, and carry on from there.
    ///
    /// With `--agent` or `--account` the conversation moves: the new agent
    /// cannot continue the old one's thread, so it is handed a digest of the
    /// transcript with its first prompt.
    Fork {
        session: String,
        /// The transcript position to fork at. Defaults to all of it.
        #[arg(long)]
        after: Option<u64>,
        /// Carry the conversation over to another agent: claude, codex.
        #[arg(long)]
        agent: Option<String>,
        /// The model for the fork. Defaults to the agent's own.
        #[arg(long)]
        model: Option<String>,
        /// The login the fork runs on, by id.
        #[arg(long)]
        account: Option<String>,
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
enum ReviewCommand {
    /// Leave a comment on a file, and a line of it.
    Add {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The file, relative to the worktree root.
        path: String,
        /// What is wrong with it.
        text: String,
        /// The line it is about. Omitted, the comment is about the file.
        #[arg(long)]
        line: Option<u32>,
    },
    /// Every comment waiting, in reading order.
    List { workspace: String },
    /// Take one comment back.
    Remove { comment: String },
    /// Send the batch to a session's agent as one message.
    Send { workspace: String, session: String },
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
        Command::Mcp => mcp(&paths),
        Command::Daemon(command) => daemon(&paths, command, cli.json),
        Command::Account(AccountCommand::Login { id }) => account_login(&paths, &id),
        Command::Commit {
            workspace,
            generate: true,
            staged,
            agent,
            ..
        } => commit_generated(&paths, &workspace, staged, agent, cli.json),
        Command::Workspace(WorkspaceCommand::Index { workspace }) => {
            workspace_index(&paths, &workspace)
        }
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

/// Speak MCP on stdin and stdout until the agent closes them.
///
/// Line-delimited JSON, one message per line, which is the stdio transport the
/// spec describes. Every tool call becomes a daemon request, so an agent has
/// exactly the capabilities a person does (`AGENTS.md` rule 3).
///
/// The daemon is connected to lazily, on the first call rather than at start:
/// an agent that lists the tools and never uses one should not have started a
/// daemon by asking.
fn mcp(paths: &Paths) -> Result<()> {
    use ginka_core::mcp;
    use std::io::{BufRead as _, Write as _};

    let input = std::io::stdin();
    let mut output = std::io::stdout();
    let mut client: Option<Client> = None;

    for line in input.lock().lines() {
        let line = line.context("reading the agent's message")?;
        if line.trim().is_empty() {
            continue;
        }
        let message: serde_json::Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                // Nothing to answer to: a message that did not parse has no id
                // to reply against, so the error carries a null one.
                let reply =
                    mcp::error(serde_json::Value::Null, mcp::PARSE_ERROR, error.to_string());
                writeln!(output, "{reply}")?;
                output.flush()?;
                continue;
            }
        };
        let method = message
            .get("method")
            .and_then(|method| method.as_str())
            .unwrap_or_default()
            .to_string();
        let id = message.get("id").cloned();
        // A notification has no id and takes no answer -- writing one back is
        // how a client ends up waiting for a reply to nothing.
        let Some(id) = id else {
            continue;
        };

        let reply = match method.as_str() {
            "initialize" => mcp::reply(id, mcp::server_info()),
            "tools/list" => mcp::reply(id, mcp::tool_list()),
            "ping" => mcp::reply(id, serde_json::json!({})),
            "tools/call" => {
                let empty = serde_json::json!({});
                let params = message.get("params").unwrap_or(&empty);
                let name = params
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or_default();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                match mcp::request_for(name, &arguments) {
                    Err(error) => mcp::reply(id, mcp::tool_failure(error.to_string())),
                    Ok(request) => {
                        let answered = smol::block_on(async {
                            if client.is_none() {
                                client = Some(connect(paths).await?);
                            }
                            let daemon = client.as_ref().expect("just connected");
                            daemon
                                .request(request)
                                .await
                                .map_err(|error| anyhow::anyhow!("{error}"))
                        });
                        match answered {
                            Ok(response) => {
                                mcp::reply(id, mcp::tool_result(serde_json::to_string(&response)?))
                            }
                            Err(error) => {
                                // The daemon may have gone; the next call
                                // reconnects rather than failing forever.
                                client = None;
                                mcp::reply(id, mcp::tool_failure(error.to_string()))
                            }
                        }
                    }
                }
            }
            other => mcp::error(
                id,
                mcp::METHOD_NOT_FOUND,
                format!("no method called {other}"),
            ),
        };
        writeln!(output, "{reply}")?;
        output.flush()?;
    }
    Ok(())
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
        Command::Project(ProjectCommand::Add { path, label }) => Request::AddProject {
            path: match path {
                Some(path) => path,
                None => std::env::current_dir()?,
            },
            label,
        },
        Command::Project(ProjectCommand::List) => Request::ListProjects,
        Command::Project(ProjectCommand::Search {
            project,
            query,
            limit,
        }) => Request::SearchProject {
            project: ProjectName(project),
            query,
            limit,
        },
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
        Command::Workspace(WorkspaceCommand::Archive { workspace, restore }) => {
            Request::ArchiveWorkspace {
                workspace: WorkspaceId(workspace),
                archived: !restore,
            }
        }

        Command::Agents => Request::ListAgents,
        Command::Slack(SlackCommand::Status) | Command::Slack(SlackCommand::Bindings) => {
            Request::ListConnectors
        }
        Command::Slack(SlackCommand::Allow { sender }) => Request::AllowConnectorSender {
            connector: "slack".to_string(),
            sender,
        },
        Command::Slack(SlackCommand::Test { channel }) => Request::TestConnector {
            connector: "slack".to_string(),
            channel,
        },
        Command::Account(AccountCommand::List) => Request::Accounts,
        Command::Account(AccountCommand::Add {
            id,
            provider,
            label,
        }) => Request::AddAccount {
            provider: ProviderKind::parse(&provider).ok_or_else(|| {
                anyhow::anyhow!(
                    "no provider named {provider}; one of: {}",
                    ProviderKind::ALL
                        .iter()
                        .map(|kind| kind.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?,
            label: label.unwrap_or_else(|| id.clone()),
            id: AccountId(id),
        },
        Command::Account(AccountCommand::Remove { id, delete_home }) => Request::RemoveAccount {
            id: AccountId(id),
            delete_home,
        },
        Command::Account(AccountCommand::Refresh { id }) => Request::RefreshPlanUsage {
            account: AccountId(id),
        },
        Command::Account(AccountCommand::Login { .. }) => {
            unreachable!("a login is run here, not asked of the daemon")
        }
        Command::Usage { days } => Request::Usage { days: Some(days) },
        Command::Workspace(WorkspaceCommand::Branches { workspace }) => Request::ListBranches {
            workspace: WorkspaceId(workspace),
        },
        Command::Workspace(WorkspaceCommand::Checkout {
            workspace,
            branch,
            create,
        }) => Request::CheckoutBranch {
            workspace: WorkspaceId(workspace),
            branch,
            create,
        },
        Command::Skills(SkillsCommand::List { project }) => Request::ListSkills {
            project: project.map(ProjectName),
        },
        Command::Skills(SkillsCommand::Enable { name, project }) => Request::SetSkillEnabled {
            name,
            enabled: true,
            project: project.map(ProjectName),
        },
        Command::Skills(SkillsCommand::Disable { name, project }) => Request::SetSkillEnabled {
            name,
            enabled: false,
            project: project.map(ProjectName),
        },
        Command::Commands { workspace } => Request::SlashCommands {
            workspace: WorkspaceId(workspace),
            query: None,
        },
        Command::FanOut {
            project,
            prefix,
            prompt,
            agents,
            base,
        } => Request::FanOut {
            project: ProjectName(project),
            branch_prefix: prefix,
            base,
            prompt,
            attempts: agents
                .into_iter()
                .map(|agent| match agent.split_once(':') {
                    Some((agent, model)) => Attempt {
                        agent: agent.to_string(),
                        model: Some(model.to_string()),
                        account: None,
                    },
                    None => Attempt {
                        agent,
                        model: None,
                        account: None,
                    },
                })
                .collect(),
        },
        Command::Search { workspace, query } => Request::SearchContent {
            workspace: WorkspaceId(workspace),
            query,
            limit: None,
        },
        Command::Show { workspace, path } => Request::ReadFile {
            workspace: WorkspaceId(workspace),
            path,
        },
        Command::Save {
            workspace,
            path,
            expected_revision,
            text,
        } => {
            let text = match text {
                Some(text) => text,
                None => {
                    use std::io::Read as _;
                    let mut text = String::new();
                    std::io::stdin()
                        .read_to_string(&mut text)
                        .context("reading replacement text from stdin")?;
                    text
                }
            };
            Request::WriteFile {
                workspace: WorkspaceId(workspace),
                path,
                text,
                expected_revision,
            }
        }
        Command::Attach { path } => {
            use base64::Engine as _;
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            Request::UploadAttachment {
                // The name is what the reader of a transcript sees; the daemon
                // never uses it as a path component.
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            }
        }
        Command::Files {
            workspace,
            query,
            limit,
        } => Request::WorkspaceFiles {
            workspace: WorkspaceId(workspace),
            query,
            limit,
        },
        Command::Review(ReviewCommand::Add {
            workspace,
            path,
            text,
            line,
        }) => Request::AddReviewComment {
            workspace: WorkspaceId(workspace),
            path,
            line,
            side: ginka_protocol::DiffSide::New,
            text,
        },
        Command::Review(ReviewCommand::List { workspace }) => Request::ListReviewComments {
            workspace: WorkspaceId(workspace),
        },
        Command::Review(ReviewCommand::Remove { comment }) => {
            Request::RemoveReviewComment { comment }
        }
        Command::Review(ReviewCommand::Send { workspace, session }) => {
            Request::SendReviewComments {
                workspace: WorkspaceId(workspace),
                session: SessionId(session),
            }
        }
        Command::Stage {
            workspace,
            path,
            undo,
        } => Request::StageFile {
            workspace: WorkspaceId(workspace),
            path,
            staged: !undo,
        },
        Command::Revert { workspace, path } => Request::RevertFile {
            workspace: WorkspaceId(workspace),
            path,
        },
        Command::Commit {
            workspace,
            message,
            staged,
            ..
        } => Request::Commit {
            workspace: WorkspaceId(workspace),
            message: message.context("a commit needs a message, or --generate")?,
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
            origin: None,
        },
        Command::Session(SessionCommand::Start {
            workspace,
            prompt,
            agent,
            model,
            reasoning_effort,
            service_tier,
            account,
            access,
        }) => Request::StartSession {
            workspace: WorkspaceId(workspace),
            agent,
            prompt,
            model,
            reasoning_effort,
            service_tier,
            account: account.map(AccountId),
            access_mode: access,
            origin: None,
        },
        Command::Session(SessionCommand::Send { session, text }) => Request::SendMessage {
            session: SessionId(session),
            text,
        },
        Command::Session(SessionCommand::Respond {
            session,
            request_id,
            response,
        }) => Request::RespondToAgent {
            session: SessionId(session),
            request_id,
            response,
        },
        Command::Session(SessionCommand::Options {
            session,
            model,
            reasoning_effort,
            service_tier,
        }) => Request::UpdateSessionOptions {
            session: SessionId(session),
            model,
            reasoning_effort,
            service_tier,
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
        Command::Session(SessionCommand::Fork {
            session,
            after,
            agent,
            model,
            account,
        }) => Request::ForkSession {
            session: SessionId(session),
            after,
            agent,
            model,
            account: account.map(AccountId),
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
        // Not one request each: handled in `main` before this is reached.
        Command::Doctor
        | Command::Daemon(_)
        | Command::Mcp
        | Command::Workspace(WorkspaceCommand::Index { .. }) => unreachable!("handled in main"),
    })
}

/// Ask the daemon for a commit message, wait for it to arrive as an event,
/// then commit with it.
///
/// The event stream is opened before the request is sent, so the answer
/// cannot land in the gap between them.
fn commit_generated(
    paths: &Paths,
    workspace: &str,
    staged: bool,
    agent: Option<String>,
    json: bool,
) -> Result<()> {
    let workspace = WorkspaceId(workspace.to_string());
    let response = smol::block_on(async {
        let client = connect(paths).await?;
        let events = client.events();
        client
            .request(Request::GenerateCommitMessage {
                workspace: workspace.clone(),
                agent,
                staged,
            })
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let message = loop {
            let event = events
                .recv()
                .await
                .context("the daemon closed the connection before answering")?;
            if let ginka_protocol::event::DaemonEvent::CommitMessageGenerated {
                workspace: done,
                message,
                error,
            } = event.payload
                && done == workspace
            {
                break message.ok_or_else(|| {
                    anyhow::anyhow!(error.unwrap_or_else(|| "no message was written".into()))
                })?;
            }
        };
        eprintln!("{}", message.trim_end());
        client
            .request(Request::Commit {
                workspace,
                message,
                all: !staged,
            })
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        print(response, false);
    }
    Ok(())
}

/// Run `zg index` in a workspace, here, so the next agent started in it is
/// handed zvec-grep's server (`ginka-core::tools`).
fn workspace_index(paths: &Paths, workspace: &str) -> Result<()> {
    let worktree = smol::block_on(async {
        let client = connect(paths).await?;
        match client
            .request(Request::ListWorkspaces { project: None })
            .await
        {
            Ok(Response::Workspaces { workspaces }) => workspaces
                .into_iter()
                .find(|summary| summary.id().0 == workspace)
                .map(|summary| summary.worktree.path)
                .ok_or_else(|| anyhow::anyhow!("no workspace named {workspace}")),
            Ok(other) => anyhow::bail!("unexpected answer {other:?}"),
            Err(error) => anyhow::bail!("{error}"),
        }
    })?;
    let status = std::process::Command::new("zg")
        .arg("index")
        .current_dir(&worktree)
        .status()
        .context(rust_i18n::t!("cli.index.no_zg").to_string())?;
    if !status.success() {
        anyhow::bail!("zg index exited with {status}");
    }
    println!("{}", rust_i18n::t!("cli.index.done"));
    Ok(())
}

/// `--access` as clap reads it: the three words, with the vendors' nearest
/// spellings accepted and the rest named in the refusal.
fn parse_access(text: &str) -> Result<ginka_protocol::AccessMode, String> {
    ginka_protocol::AccessMode::parse(text)
        .ok_or_else(|| format!("expected read-only, ask or auto, not {text:?}"))
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
        Response::Accounts { accounts } => print_accounts(&accounts, &[]),
        Response::Connectors { connectors } => print_connectors(&connectors),
        Response::Account { account } => print_accounts(std::slice::from_ref(&account), &[]),
        Response::PlanUsage { snapshot } => match snapshot {
            Some(snapshot) => print_plans(std::slice::from_ref(&snapshot)),
            None => println!("{}", rust_i18n::t!("cli.plan.unanswered")),
        },
        Response::Sessions { sessions } => print_sessions(&sessions),
        Response::SessionMatches { matches } => print_matches(&matches),
        Response::Session { session } => print_sessions(std::slice::from_ref(&session)),
        Response::SessionOptionsApplied { session, outcome } => {
            print_sessions(std::slice::from_ref(&session));
            let key = if outcome.absorbed() {
                "cli.session.options.absorbed"
            } else {
                "cli.session.options.restart"
            };
            eprintln!("{}", rust_i18n::t!(key));
        }
        Response::FannedOut { started, failed } => {
            print_sessions(&started);
            // On stderr: the sessions that did start are the answer, and a
            // script reading stdout should not have to filter complaints out
            // of it.
            for problem in failed {
                eprintln!("{problem}");
            }
        }
        Response::Checkpoints { checkpoints } => print_checkpoints(&checkpoints),
        Response::Changes { changes } => print_changes(&changes, patch),
        Response::Files { files } => {
            for file in files {
                println!("{}", file.path);
            }
        }
        Response::Draft { text } => println!("{text}"),
        // The reference and nothing else, so it can be interpolated straight
        // into the next command.
        Response::Attachment { attachment } => println!("{}", attachment.reference),
        // A terminal is opened by a window, which is where it is typed into;
        // printing the id is all a script can do with one.
        // The file as it is: a viewer prints what is in it, and anything
        // added here would be something a pipe has to strip back out.
        Response::FileContent { file } => print!("{}", file.text),
        // `path:line: text`, which is what every other search prints and what
        // an editor knows how to open.
        Response::Matches { matches } => {
            for hit in matches {
                println!("{}:{}: {}", hit.path, hit.line, hit.text);
            }
        }
        Response::WorkspaceMatches { files, matches } => {
            for hit in files {
                println!("{}:{}", hit.workspace, hit.path);
            }
            for hit in matches {
                println!("{}:{}:{}: {}", hit.workspace, hit.path, hit.line, hit.text);
            }
        }
        Response::Terminal { terminal } => println!("{terminal}"),
        Response::Terminals { terminals } => {
            for terminal in terminals {
                println!("{}\t{}", terminal.id, terminal.title);
            }
        }
        // Escapes and all: what a terminal printed is only meaningful to
        // something that renders a terminal, and rewriting it here would be
        // guessing at a screen this end does not have.
        Response::TerminalHistory { data } => print!("{data}"),
        Response::Usage {
            by_day,
            by_agent,
            by_account,
            plans,
        } => print_usage(&by_day, &by_agent, &by_account, &plans),
        Response::ReviewComments { comments } => {
            if comments.is_empty() {
                println!("{}", rust_i18n::t!("cli.review.empty"));
            }
            for comment in comments {
                println!(
                    "{:<34} {}{}  {}",
                    comment.id,
                    comment.path,
                    comment
                        .line
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default(),
                    comment.text
                );
            }
        }
        Response::Branches { branches } => {
            for branch in branches {
                println!(
                    "{} {:<40} {}",
                    if branch.current { "*" } else { " " },
                    branch.name,
                    match (&branch.checked_out_at, branch.current) {
                        (Some(path), false) => path.display().to_string(),
                        _ => String::new(),
                    }
                );
            }
        }
        Response::Skills { skills, truncated } => {
            if skills.is_empty() {
                println!("{}", rust_i18n::t!("cli.skills.empty"));
            }
            for skill in &skills {
                println!(
                    "{:<3} {:<28} {}",
                    if skill.enabled { "on" } else { "off" },
                    skill.name,
                    skill.description.as_deref().unwrap_or_default()
                );
                for install in &skill.installs {
                    println!(
                        "    {:<8} {:<10} {}{}",
                        match install.scope {
                            ginka_protocol::model::SkillScope::Project => "project",
                            ginka_protocol::model::SkillScope::User => "user",
                        },
                        install.root_label,
                        install.directory.display(),
                        if install.enabled { "" } else { " (off)" }
                    );
                }
            }
            if truncated {
                eprintln!("{}", rust_i18n::t!("cli.skills.truncated"));
            }
        }
        Response::Commands { commands } => {
            if commands.is_empty() {
                println!("{}", rust_i18n::t!("cli.commands.empty"));
            }
            for command in commands {
                println!(
                    "/{:<24} {:<8} {}",
                    command.name,
                    match command.scope {
                        ginka_protocol::CommandScope::Project => "project",
                        ginka_protocol::CommandScope::User => "user",
                    },
                    command.description
                );
            }
        }
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
fn print_usage(
    by_day: &[UsageRow],
    by_agent: &[UsageRow],
    by_account: &[UsageRow],
    plans: &[PlanSnapshot],
) {
    if by_day.is_empty() {
        println!("{}", rust_i18n::t!("cli.usage.empty"));
        print_plans(plans);
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
    println!();
    for entry in by_account {
        row(entry);
    }
    if !plans.is_empty() {
        println!();
        print_plans(plans);
    }
}

/// One line per login: id, provider, label, signed in, and either the
/// tightest of its windows or where it lives.
fn print_accounts(accounts: &[Account], plans: &[PlanSnapshot]) {
    let now = now();
    for account in accounts {
        let state = match account.signed_in {
            Some(true) => rust_i18n::t!("cli.agent.ready").to_string(),
            Some(false) => rust_i18n::t!("cli.agent.signed_out").to_string(),
            None => String::new(),
        };
        let plan = plans
            .iter()
            .find(|snapshot| snapshot.account == account.id)
            .map(|snapshot| plan_line(snapshot, now))
            .unwrap_or_default();
        println!(
            "{:<18} {:<8} {:<16} {:<12} {}",
            account.id.0,
            account.provider.as_str(),
            account.label,
            state,
            if plan.is_empty() {
                account
                    .home
                    .as_ref()
                    .map(|home| home.display().to_string())
                    .unwrap_or_default()
            } else {
                plan
            }
        );
    }
}

/// One connector per block: its state, then one line per bound channel.
fn print_connectors(connectors: &[ConnectorState]) {
    for connector in connectors {
        let state = if !connector.enabled {
            rust_i18n::t!("cli.connector.disabled").to_string()
        } else if connector.connected {
            rust_i18n::t!(
                "cli.connector.connected",
                age = age_label(now() - connector.since.unwrap_or_else(now))
            )
            .to_string()
        } else {
            rust_i18n::t!("cli.connector.disconnected").to_string()
        };
        println!("{:<8} {state}", connector.id);
        if let Some(error) = &connector.last_error {
            println!(
                "         {}",
                rust_i18n::t!("cli.connector.error", error = error)
            );
        }
        if connector.bindings.is_empty() {
            println!("         {}", rust_i18n::t!("cli.connector.bindings.empty"));
        }
        for binding in &connector.bindings {
            println!(
                "         {:<14} {:<24} {:<8} {:<8} {}",
                binding.channel, binding.target, binding.agent, binding.trigger, binding.worktree
            );
        }
    }
}

/// Every window of every reading, with how old the reading is.
fn print_plans(plans: &[PlanSnapshot]) {
    let now = now();
    for snapshot in plans {
        println!("{:<18} {}", snapshot.account.0, plan_line(snapshot, now));
    }
}

/// `pro · 5h 92% resets in 40m · week 40% resets in 3d 2h · 5m ago`
fn plan_line(snapshot: &PlanSnapshot, now: i64) -> String {
    let mut parts: Vec<String> = snapshot.usage.plan.iter().cloned().collect();
    for window in &snapshot.usage.windows {
        let reset = window.reset_label(now);
        parts.push(if reset.is_empty() {
            format!("{} {:.0}%", window.label, window.used_percent)
        } else {
            format!("{} {:.0}% {reset}", window.label, window.used_percent)
        });
    }
    parts.push(
        rust_i18n::t!("cli.plan.age", age = age_label(now - snapshot.observed_at)).to_string(),
    );
    parts.join(" · ")
}

/// How long ago, in the coarsest unit that is not zero.
fn age_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// Unix seconds, for the ages the account list shows.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Run the vendor's own sign-in for a login, in this terminal.
///
/// The daemon says what to run and which directory to point it at; the
/// browser round-trip and the token are the vendor's, and this terminal is
/// where the user already is. Afterwards the daemon is asked again whether
/// the login worked, which is how the answer reaches every window too.
fn account_login(paths: &Paths, id: &str) -> Result<()> {
    let accounts = smol::block_on(async {
        let client = connect(paths).await?;
        match client.request(Request::Accounts).await {
            Ok(Response::Accounts { accounts }) => Ok(accounts),
            Ok(other) => anyhow::bail!("unexpected answer {other:?}"),
            Err(error) => anyhow::bail!("{error}"),
        }
    })?;
    let account = accounts
        .iter()
        .find(|account| account.id.0 == id)
        .ok_or_else(|| anyhow::anyhow!("no account named {id}; `ginka account list` names them"))?;
    let login = account.login.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no sign-in command; sign in with the CLI itself",
            account.provider
        )
    })?;
    let status = std::process::Command::new(&login.program)
        .args(&login.args)
        .envs(login.env.iter().map(|(key, value)| (key, value)))
        .status()
        .with_context(|| format!("running {}", login.program))?;
    if !status.success() {
        anyhow::bail!("{} exited with {status}", login.program);
    }
    let signed_in = smol::block_on(async {
        let client = connect(paths).await?;
        match client.request(Request::Accounts).await {
            Ok(Response::Accounts { accounts }) => Ok(accounts
                .into_iter()
                .find(|account| account.id.0 == id)
                .and_then(|account| account.signed_in)),
            Ok(other) => anyhow::bail!("unexpected answer {other:?}"),
            Err(error) => anyhow::bail!("{error}"),
        }
    })?;
    println!(
        "{}",
        match signed_in {
            Some(true) => rust_i18n::t!("cli.account.login.done"),
            Some(false) => rust_i18n::t!("cli.account.login.still_out"),
            None => rust_i18n::t!("cli.account.login.unknown"),
        }
    );
    Ok(())
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
            TranscriptPayload::Response { request_id, text } => {
                format!("{:>4}  you  [{request_id}] {text}", entry.seq)
            }
            TranscriptPayload::Agent { event } => {
                format!("{:>4}  {}", entry.seq, describe(event))
            }
        }
    }

    fn describe(event: &AgentEvent) -> String {
        match event {
            AgentEvent::TextDelta { text } => text.clone(),
            AgentEvent::Reasoning { text } => format!("(thinking) {text}"),
            AgentEvent::ToolCall { activity } => {
                format!("[{}] {}", activity.kind_str(), activity.title)
            }
            AgentEvent::ToolResult { activity } => {
                let marker = if activity.failed { "!" } else { " " };
                let first = activity
                    .detail
                    .as_deref()
                    .and_then(|detail| detail.lines().next())
                    .unwrap_or("");
                format!("[result]{marker} {first}")
            }
            AgentEvent::SubagentStarted { title, .. } => {
                format!("[subagent] {title}")
            }
            AgentEvent::SubagentStep { step, .. } => {
                let marker = match step.status {
                    Some(ginka_protocol::SubagentStepStatus::Running) => "…",
                    Some(ginka_protocol::SubagentStepStatus::Completed) => "✓",
                    Some(ginka_protocol::SubagentStepStatus::Failed) => "!",
                    None => "·",
                };
                format!("[subagent {marker}] {}", step.text)
            }
            AgentEvent::SubagentFinished {
                summary, failed, ..
            } => {
                let marker = if *failed { "!" } else { "✓" };
                format!(
                    "[subagent {marker}] {}",
                    summary.as_deref().unwrap_or_default()
                )
            }
            AgentEvent::AskUser { question, .. } => format!("? {question}"),
            AgentEvent::PlanProposal { plan, .. } => format!("plan: {plan}"),
            AgentEvent::Usage { usage } => format!(
                "usage: {} in, {} out",
                usage.input_tokens, usage.output_tokens
            ),
            AgentEvent::PlanUsage { usage } => match usage.tightest() {
                Some(window) => format!("plan: {} {:.0}% used", window.label, window.used_percent),
                None => String::new(),
            },
            AgentEvent::TurnEnd { turn } => format!("-- end of turn {turn} --"),
            AgentEvent::SessionResult { state, summary } => format!(
                "== {} {}",
                state.as_str(),
                summary.clone().unwrap_or_default()
            ),
            AgentEvent::Connected { model, .. } => match model {
                Some(model) => format!("-- connected ({model}) --"),
                None => "-- connected --".to_string(),
            },
            AgentEvent::AgentTitle { title } => format!("-- titled: {title} --"),
            AgentEvent::Permission { request, .. } => format!("? permission: {request}"),
            AgentEvent::SteerRejected { reason } => format!(
                "-- steer refused{} --",
                reason
                    .as_deref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            ),
            AgentEvent::ProcessExited { code } => match code {
                Some(code) => format!("-- agent exited ({code}) --"),
                None => "-- agent exited --".to_string(),
            },
            // Said out loud rather than dropped: a shape this build does not
            // know is how a vendor's format change first shows up.
            AgentEvent::Unsupported { shape } => format!("-- not understood: {shape} --"),
            // Nothing a transcript reader needs to see.
            AgentEvent::Commands { .. } | AgentEvent::TurnStarted | AgentEvent::SteerAccepted => {
                String::new()
            }
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
                        activity: {
                            let mut activity = ginka_protocol::event::ActivityItem::from_tool(
                                Some("t".into()),
                                "Read",
                                &serde_json::json!({}),
                            );
                            activity.complete_with("no such file\nmore", true);
                            activity
                        },
                    },
                },
            };
            let line = transcript_line(&entry);
            assert!(line.contains("[result]!"), "{line}");
            assert!(!line.contains("more"), "only the first line: {line}");
        }
    }
}
