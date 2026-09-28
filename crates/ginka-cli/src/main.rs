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
use ginka_protocol::{AccountId, CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
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
    /// Read a stored image as a data URL after the daemon verifies its format.
    AttachmentImage {
        /// The reference printed by `ginka attach`.
        reference: String,
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
    /// Open a workspace file in an editor on the daemon host.
    OpenEditor {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
        /// One-based line to focus when the editor supports it.
        #[arg(long)]
        line: Option<u32>,
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
        #[arg(long, conflicts_with_all = ["unstaged", "since", "turn"])]
        staged: bool,
        /// Show only edits that are not staged yet.
        #[arg(long, conflicts_with_all = ["since", "turn"])]
        unstaged: bool,
        /// Show what has happened since a checkpoint, by its id.
        #[arg(long, conflicts_with = "turn")]
        since: Option<String>,
        /// Show only what one completed turn changed, by its checkpoint id.
        #[arg(long, conflicts_with_all = ["staged", "unstaged", "since", "commit"])]
        turn: Option<String>,
        /// Show what one commit did, by the id `history` prints.
        #[arg(long, conflicts_with_all = ["since", "turn", "staged", "unstaged"])]
        commit: Option<String>,
        /// Print the diff itself rather than a summary.
        #[arg(long)]
        patch: bool,
        /// Unchanged lines around each edit, up to 25.
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(0..=25))]
        context: u8,
    },
    /// Show recent commits in a workspace, newest first.
    History {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Maximum commits to print.
        #[arg(long, default_value_t = 50)]
        limit: u32,
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
    /// Put one exact diff hunk into the next commit, or take it back out.
    StageHunk {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
        /// Complete `@@` header printed by `changes --patch`.
        header: String,
        /// Take the hunk back out instead of putting it in.
        #[arg(long)]
        undo: bool,
    },
    /// Throw away one exact unstaged diff hunk.
    ///
    /// This cannot be undone through git. A stale hunk header is refused.
    RevertHunk {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The path, relative to the worktree root.
        path: String,
        /// Complete `@@` header printed by `changes --unstaged --patch`.
        header: String,
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
    /// Hand a stopped merge, rebase or cherry-pick's conflicts to an agent
    /// to resolve and finish.
    Resolve {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Start a new conversation on this agent instead of following up in
        /// the workspace's latest one.
        #[arg(long)]
        agent: Option<String>,
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
        /// Fold the work into the last commit instead of making a new one.
        /// Without a message the commit keeps its own. Refused once that
        /// commit has been pushed.
        #[arg(long, conflicts_with = "generate")]
        amend: bool,
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
    /// Fetch and fast-forward a clean workspace branch from its upstream.
    Pull {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// Bring a workspace branch level with its remote: publish it if it was
    /// never pushed, otherwise fast-forward, then push what is ahead.
    Sync {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
    },
    /// Push a workspace's branch and open a pull request for it with `gh`,
    /// titled from its commits. Prints the pull request's address.
    Pr {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// Open it as a draft.
        #[arg(long)]
        draft: bool,
    },
    /// Markdown notes, kept by the daemon.
    #[command(subcommand)]
    Notes(NotesCommand),
    /// Tickets: work an agent handed over, to start in its own session.
    #[command(subcommand)]
    Tickets(TicketsCommand),
    /// Saved shell commands and prompts, run in a workspace.
    #[command(subcommand)]
    Quick(QuickCommand),
    /// The daemon's settings: show them, or change one.
    #[command(subcommand)]
    Settings(SettingsCommand),
    /// The daemon's terminals in a workspace: open one, type into it, read
    /// what it printed, close it.
    #[command(subcommand)]
    Terminal(TerminalCommand),
    /// Prompts and commands run on a cron schedule, on this machine's clock.
    #[command(subcommand)]
    Cron(CronCommand),
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
    /// Name a project the way you group it; an empty label clears it.
    Label { project: String, label: String },
    /// Put a project at a position in the order, 0 first.
    Move { project: String, index: u32 },
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
    /// Merge a workspace's branch into another — by default the branch the
    /// project is on. A conflict is aborted and named.
    Merge {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// The branch to merge into.
        #[arg(long)]
        into: Option<String>,
        /// Commit the workspace's uncommitted work with this message first.
        #[arg(long, short)]
        message: Option<String>,
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
    /// Write the line of status the sidebar shows under a workspace, or
    /// clear it by giving none.
    Status {
        /// The workspace id, as shown by `workspace list`.
        workspace: String,
        /// One line: what the work is doing, or waiting on.
        note: Option<String>,
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
    /// Select the login future sessions of its provider use.
    Select { id: String },
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
enum QuickCommand {
    /// List a project's quick commands and the global ones.
    List {
        #[arg(long)]
        project: Option<String>,
    },
    /// Save a shell command (`--shell`) or a prompt (`--prompt`).
    Add {
        name: String,
        #[arg(long, conflicts_with = "prompt", required_unless_present = "prompt")]
        shell: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        /// The project it belongs to; every project when omitted.
        #[arg(long)]
        project: Option<String>,
    },
    /// Forget a quick command.
    Remove { id: String },
    /// Run a shell quick command in a new terminal in the workspace.
    Run { workspace: String, id: String },
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Print the daemon's settings as JSON, environment values hidden.
    Show,
    /// List the shipped providers and their enabled state and executable override.
    Providers,
    /// Configure a shipped provider for future turns.
    Provider {
        provider: String,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
        #[arg(long, conflicts_with = "clear_program")]
        program: Option<String>,
        #[arg(long)]
        clear_program: bool,
    },
    /// Change one top-level setting. The value is JSON — `false`, `7`,
    /// `["codex"]` — and anything that is not is taken as a string.
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum TerminalCommand {
    /// The terminals running in a workspace.
    List { workspace: String },
    /// Open a shell in a workspace, and print its id.
    Open { workspace: String },
    /// Type a line into a terminal, and press Enter unless told not to.
    Send {
        terminal: String,
        text: String,
        #[arg(long)]
        no_enter: bool,
    },
    /// Print what a terminal has shown lately, as plain text.
    Read { terminal: String },
    /// Close a terminal and stop its shell.
    Close { terminal: String },
}

#[derive(Subcommand)]
enum CronCommand {
    /// List scheduled jobs, with when each fires next and how it last went.
    List {
        #[arg(long)]
        project: Option<String>,
    },
    /// Schedule a shell command (`--shell`) or a prompt for an agent
    /// (`--prompt` with `--agent`).
    Add {
        project: String,
        name: String,
        /// Five cron fields, or `@hourly`, `@daily`, `@weekly`, `@monthly`.
        #[arg(long, conflicts_with = "at", required_unless_present = "at")]
        schedule: Option<String>,
        /// Run once at an RFC 3339 timestamp with a time zone.
        #[arg(
            long,
            conflicts_with = "schedule",
            required_unless_present = "schedule"
        )]
        at: Option<String>,
        #[arg(long, conflicts_with = "prompt", required_unless_present = "prompt")]
        shell: Option<String>,
        #[arg(long, requires = "agent")]
        prompt: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        /// A workspace id; the project's own checkout when omitted.
        #[arg(long)]
        workspace: Option<String>,
        /// A shell command run in the checkout before each scheduled firing;
        /// a non-zero exit skips that firing (`gh pr list … | grep -q .`).
        #[arg(long)]
        precheck: Option<String>,
        /// Save it switched off.
        #[arg(long)]
        disabled: bool,
    },
    /// Forget a scheduled job and its history.
    Remove { id: i64 },
    /// Fire a job now, as its schedule would.
    Run { id: i64 },
    /// A job's firings, most recent first.
    Runs {
        id: i64,
        #[arg(long)]
        limit: Option<u32>,
    },
}

#[derive(Subcommand)]
enum NotesCommand {
    /// List notes, most recently touched first.
    List {
        /// Only this project's notes.
        #[arg(long)]
        project: Option<String>,
        /// Search titles, bodies and tags.
        #[arg(long)]
        query: Option<String>,
        /// Match one tag exactly.
        #[arg(long)]
        tag: Option<String>,
    },
    /// Print one note's markdown.
    Show { id: String },
    /// Write a new note. The body is read from stdin when `--body` is absent.
    Add {
        /// What it is called. Its first line otherwise.
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long)]
        body: Option<String>,
        /// The project it belongs to.
        #[arg(long)]
        project: Option<String>,
        /// Add a tag; repeat for several tags.
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Replace a note's title and body. The body is read from stdin when
    /// `--body` is absent.
    Edit {
        id: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long)]
        body: Option<String>,
        /// Replace tags; repeat for several tags.
        #[arg(long = "tag", conflicts_with = "clear_tags")]
        tags: Vec<String>,
        /// Remove every tag.
        #[arg(long)]
        clear_tags: bool,
    },
    /// Forget a note.
    Remove { id: String },
}

#[derive(Subcommand)]
enum TicketsCommand {
    /// List tickets, newest first: open ones unless `--all`.
    List {
        /// Only this workspace's tickets.
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Raise a ticket. The prompt is read from stdin when `--prompt` is
    /// absent.
    Raise {
        workspace: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "")]
        summary: String,
        #[arg(long)]
        prompt: Option<String>,
        /// Raise it as this session.
        #[arg(long)]
        from: Option<String>,
    },
    /// Start an open ticket in a new session.
    Start {
        ticket: String,
        /// A driver id; the raising session's agent otherwise.
        #[arg(long)]
        agent: Option<String>,
        /// Cut a new worktree on this branch for it.
        #[arg(long)]
        branch: Option<String>,
    },
    /// Decide against an open ticket.
    Dismiss { ticket: String },
}

#[derive(Subcommand)]
enum SkillsCommand {
    /// List every skill, grouped across the places it was installed.
    List {
        /// Only this project's skills, plus the user's own.
        #[arg(long)]
        project: Option<String>,
    },
    /// Create a shared skill under the user or a registered project.
    Create {
        name: String,
        /// Short description shown in skill pickers.
        #[arg(long)]
        description: String,
        /// Markdown instructions for the agent.
        #[arg(long)]
        body: String,
        /// Create under this registered project instead of the user home.
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
    /// Install the skills that teach an agent to drive Ginka into Claude
    /// Code's and Codex's skills directories.
    Install {
        /// Replace a different file of the same name.
        #[arg(long)]
        force: bool,
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
        /// Which login to run on, by id. The provider's active account otherwise.
        #[arg(long)]
        account: Option<String>,
        /// What the agent may touch: read-only, ask (edit freely, and be
        /// asked before a command runs) or auto (edit and run). Defaults to
        /// ask.
        #[arg(long, value_parser = parse_access)]
        access: Option<ginka_protocol::AccessMode>,
    },
    /// Send a follow-up. Queued if the agent is still working.
    Send {
        session: String,
        text: String,
        /// Send it as this session, which the receiver is told, with how to
        /// answer.
        #[arg(long)]
        from: Option<String>,
    },
    /// List follow-ups waiting behind the active turn.
    Queue { session: String },
    /// Replace one queued follow-up without moving it.
    QueueEdit {
        session: String,
        id: u64,
        text: String,
    },
    /// Remove one queued follow-up.
    QueueRemove { session: String, id: u64 },
    /// Move one queued follow-up to a zero-based position.
    QueueMove {
        session: String,
        id: u64,
        index: u32,
    },
    /// Inject one queued follow-up into the active turn when supported.
    QueueSendNow { session: String, id: u64 },
    /// Edit a sent prompt, by the position `session log` shows it at, and
    /// run the conversation again from it in a new session.
    Edit {
        session: String,
        seq: u64,
        text: String,
    },
    /// Queue a follow-up even where the running turn could take it now.
    QueueAdd { session: String, text: String },
    /// Stop the running turn and send this queued follow-up next.
    QueueInterrupt { session: String, id: u64 },
    /// Hold the queue, or let it go with `--resume`.
    QueuePause {
        session: String,
        #[arg(long)]
        resume: bool,
    },
    /// Throw away every queued follow-up.
    QueueClear { session: String },
    /// Compact an idle provider conversation's context.
    Compact { session: String },
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
    /// List conversations an agent's own CLI started in a workspace's
    /// directory, which Ginka can adopt.
    Cli {
        /// The workspace, by id.
        workspace: String,
    },
    /// Bring a conversation started in an agent's CLI into Ginka and carry
    /// on from it: the next turn resumes the same thread.
    Adopt {
        /// The workspace it was started in, by id.
        workspace: String,
        /// The agent whose CLI started it: claude, codex.
        agent: String,
        /// The vendor's id for it, as `ginka session cli` lists it.
        id: String,
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
            let showing_note = match &command {
                Command::Notes(NotesCommand::Show { id }) => Some(id.clone()),
                _ => None,
            };
            let request = request_for(command)?;
            let response = smol::block_on(async {
                let client = connect(&paths).await?;
                client
                    .request(request)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))
            })?;
            let response = match (showing_note, response) {
                (Some(id), Response::Notes { notes }) => {
                    let note = notes
                        .into_iter()
                        .find(|note| note.id == id)
                        .ok_or_else(|| anyhow::anyhow!("no note with id {id}"))?;
                    if !cli.json {
                        println!("{}", note.body);
                        return Ok(());
                    }
                    Response::Note { note }
                }
                (_, response) => response,
            };
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
    // Set by the daemon when it started this bridge for a session: what signs
    // this agent's tickets and messages.
    let caller = std::env::var(ginka_core::tools::SESSION_ENV)
        .ok()
        .filter(|id| !id.trim().is_empty())
        .map(ginka_protocol::SessionId);

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
            "initialize" => mcp::reply(id, mcp::server_info_as(caller.as_ref())),
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
                match mcp::request_as(name, &arguments, caller.as_ref()) {
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
/// A note's body: the flag's, or everything on stdin, so `ginka notes add <
/// steps.md` keeps a file as a note.
fn body_or_stdin(body: Option<String>) -> Result<String> {
    use std::io::Read as _;
    match body {
        Some(body) => Ok(body),
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            Ok(text)
        }
    }
}

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
        Command::Project(ProjectCommand::Label { project, label }) => Request::SetProjectLabel {
            project: ProjectName(project),
            label,
        },
        Command::Project(ProjectCommand::Move { project, index }) => Request::MoveProject {
            project: ProjectName(project),
            index,
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
        Command::Workspace(WorkspaceCommand::Status { workspace, note }) => {
            Request::SetWorkspaceStatus {
                workspace: Some(WorkspaceId(workspace)),
                path: None,
                note,
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
        Command::Account(AccountCommand::Select { id }) => {
            Request::SelectAccount { id: AccountId(id) }
        }
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
        Command::Workspace(WorkspaceCommand::Merge {
            workspace,
            into,
            message,
        }) => Request::MergeWorkspace {
            workspace: WorkspaceId(workspace),
            into,
            message,
        },
        Command::Skills(SkillsCommand::Install { force }) => {
            Request::InstallBundledSkills { force }
        }
        Command::Skills(SkillsCommand::List { project }) => Request::ListSkills {
            project: project.map(ProjectName),
        },
        Command::Skills(SkillsCommand::Create {
            name,
            description,
            body,
            project,
        }) => Request::CreateSkill {
            name,
            description,
            body,
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
        Command::OpenEditor {
            workspace,
            path,
            line,
        } => Request::OpenExternalEditor {
            workspace: WorkspaceId(workspace),
            path,
            line,
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
        Command::AttachmentImage { reference } => Request::ReadAttachmentImage { reference },
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
        Command::StageHunk {
            workspace,
            path,
            header,
            undo,
        } => Request::StageHunk {
            workspace: WorkspaceId(workspace),
            path,
            header,
            staged: !undo,
        },
        Command::RevertHunk {
            workspace,
            path,
            header,
        } => Request::RevertHunk {
            workspace: WorkspaceId(workspace),
            path,
            header,
        },
        Command::Resolve { workspace, agent } => Request::ResolveConflicts {
            workspace: WorkspaceId(workspace),
            agent,
        },
        Command::Revert { workspace, path } => Request::RevertFile {
            workspace: WorkspaceId(workspace),
            path,
        },
        Command::Commit {
            workspace,
            message,
            staged,
            amend,
            ..
        } => Request::Commit {
            workspace: WorkspaceId(workspace),
            message: match message {
                Some(message) => message,
                None if amend => String::new(),
                None => anyhow::bail!("a commit needs a message, or --generate"),
            },
            all: !staged,
            amend,
        },
        Command::Push { workspace } => Request::Push {
            workspace: WorkspaceId(workspace),
        },
        Command::Pull { workspace } => Request::Pull {
            workspace: WorkspaceId(workspace),
        },
        Command::Sync { workspace } => Request::Sync {
            workspace: WorkspaceId(workspace),
        },
        Command::Changes {
            workspace,
            staged,
            unstaged,
            since,
            turn,
            commit,
            context,
            ..
        } => Request::WorkspaceChanges {
            workspace: WorkspaceId(workspace),
            context_lines: Some(context),
            source: match (staged, unstaged, since, turn) {
                _ if commit.is_some() => ChangeSource::Commit {
                    commit: commit.unwrap_or_default(),
                },
                (_, _, _, Some(checkpoint)) => ChangeSource::Turn {
                    checkpoint: CheckpointId(checkpoint),
                },
                (_, _, Some(checkpoint), None) => ChangeSource::SinceCheckpoint {
                    checkpoint: CheckpointId(checkpoint),
                },
                (true, false, None, None) => ChangeSource::Staged,
                (false, true, None, None) => ChangeSource::Unstaged,
                (false, false, None, None) => ChangeSource::Uncommitted,
                _ => unreachable!("clap rejects conflicting diff sources"),
            },
        },
        Command::Pr { workspace, draft } => Request::CreatePullRequest {
            workspace: WorkspaceId(workspace),
            draft,
        },
        Command::Notes(NotesCommand::List {
            project,
            query,
            tag,
        }) => Request::ListNotes {
            project: project.map(ProjectName),
            query,
            tag,
        },
        // One note is the list read and filtered: the window never needs one
        // alone, so the daemon has no request for it.
        Command::Notes(NotesCommand::Show { .. }) => Request::ListNotes {
            project: None,
            query: None,
            tag: None,
        },
        Command::Notes(NotesCommand::Add {
            title,
            body,
            project,
            tags,
        }) => Request::SaveNote {
            id: None,
            project: project.map(ProjectName),
            title,
            body: body_or_stdin(body)?,
            tags: Some(tags),
        },
        Command::Notes(NotesCommand::Edit {
            id,
            title,
            body,
            tags,
            clear_tags,
        }) => Request::SaveNote {
            id: Some(id),
            project: None,
            title,
            body: body_or_stdin(body)?,
            tags: (clear_tags || !tags.is_empty()).then_some(tags),
        },
        Command::Notes(NotesCommand::Remove { id }) => Request::RemoveNote { id },
        Command::Settings(SettingsCommand::Show) => Request::DaemonSettings,
        Command::Settings(SettingsCommand::Providers) => Request::ListProviderSettings,
        Command::Settings(SettingsCommand::Provider {
            provider,
            enable,
            disable,
            program,
            clear_program,
        }) => Request::UpdateProviderSettings {
            provider: ProviderKind::parse(&provider)
                .ok_or_else(|| anyhow::anyhow!("unknown provider: {provider}"))?,
            enabled: if enable {
                Some(true)
            } else if disable {
                Some(false)
            } else {
                None
            },
            program,
            clear_program,
        },
        Command::Settings(SettingsCommand::Set { key, value }) => Request::UpdateDaemonSettings {
            key,
            value: if serde_json::from_str::<serde_json::Value>(&value).is_ok() {
                value
            } else {
                serde_json::Value::String(value).to_string()
            },
        },
        Command::Terminal(TerminalCommand::List { workspace }) => Request::WorkspaceTerminals {
            workspace: WorkspaceId(workspace),
        },
        Command::Terminal(TerminalCommand::Open { workspace }) => Request::OpenTerminal {
            workspace: WorkspaceId(workspace),
            rows: 24,
            cols: 100,
        },
        Command::Terminal(TerminalCommand::Send {
            terminal,
            text,
            no_enter,
        }) => Request::WriteTerminal {
            terminal: TerminalId(terminal),
            data: if no_enter { text } else { format!("{text}\r") },
        },
        Command::Terminal(TerminalCommand::Read { terminal }) => Request::TerminalHistory {
            terminal: TerminalId(terminal),
        },
        Command::Terminal(TerminalCommand::Close { terminal }) => Request::CloseTerminal {
            terminal: TerminalId(terminal),
        },
        Command::Cron(CronCommand::List { project }) => Request::ListCronJobs {
            project: project.map(ProjectName),
        },
        Command::Cron(CronCommand::Add {
            project,
            name,
            schedule,
            at,
            shell,
            prompt,
            agent,
            workspace,
            precheck,
            disabled,
        }) => {
            let (via, body) = match (shell, prompt) {
                (Some(shell), _) => (ginka_protocol::model::CronVia::Terminal, shell),
                (None, Some(prompt)) => (ginka_protocol::model::CronVia::Chat, prompt),
                (None, None) => unreachable!("clap requires one of --shell and --prompt"),
            };
            Request::SaveCronJob {
                id: None,
                project: ProjectName(project),
                workspace: workspace.map(WorkspaceId),
                name,
                schedule: at.map_or_else(
                    || schedule.expect("clap requires --schedule or --at"),
                    |at| format!("@once {at}"),
                ),
                via,
                agent,
                body,
                precheck,
                enabled: !disabled,
            }
        }
        Command::Cron(CronCommand::Remove { id }) => Request::RemoveCronJob { id },
        Command::Cron(CronCommand::Run { id }) => Request::RunCronJob { id },
        Command::Cron(CronCommand::Runs { id, limit }) => Request::CronRuns { id, limit },
        Command::Tickets(TicketsCommand::List { workspace, all }) => Request::ListTickets {
            workspace: workspace.map(WorkspaceId),
            all,
        },
        Command::Tickets(TicketsCommand::Raise {
            workspace,
            title,
            summary,
            prompt,
            from,
        }) => Request::RaiseTicket {
            workspace: Some(WorkspaceId(workspace)),
            from_session: from.map(SessionId),
            title,
            summary,
            prompt: body_or_stdin(prompt)?,
        },
        Command::Tickets(TicketsCommand::Start {
            ticket,
            agent,
            branch,
        }) => Request::StartTicket {
            ticket,
            agent,
            branch,
        },
        Command::Tickets(TicketsCommand::Dismiss { ticket }) => Request::DismissTicket { ticket },
        Command::Quick(QuickCommand::List { project }) => Request::ListQuickCommands {
            project: project.map(ProjectName),
        },
        Command::Quick(QuickCommand::Add {
            name,
            shell,
            prompt,
            project,
        }) => {
            let (kind, body) = match (shell, prompt) {
                (Some(shell), _) => (ginka_protocol::model::QuickCommandKind::Shell, shell),
                (None, Some(prompt)) => (ginka_protocol::model::QuickCommandKind::Prompt, prompt),
                (None, None) => unreachable!("clap requires one of --shell and --prompt"),
            };
            Request::SaveQuickCommand {
                id: None,
                project: project.map(ProjectName),
                name,
                kind,
                body,
            }
        }
        Command::Quick(QuickCommand::Remove { id }) => Request::RemoveQuickCommand { id },
        Command::Quick(QuickCommand::Run { workspace, id }) => Request::RunQuickCommand {
            workspace: WorkspaceId(workspace),
            id,
            rows: 24,
            cols: 100,
        },
        Command::History { workspace, limit } => Request::WorkspaceHistory {
            workspace: WorkspaceId(workspace),
            limit: Some(limit),
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
        Command::Session(SessionCommand::Send {
            session,
            text,
            from: None,
        }) => Request::SendMessage {
            session: SessionId(session),
            text,
        },
        Command::Session(SessionCommand::Send {
            session,
            text,
            from: Some(from),
        }) => Request::MessageSession {
            from: SessionId(from),
            to: SessionId(session),
            text,
        },
        Command::Session(SessionCommand::Queue { session }) => Request::QueuedMessages {
            session: SessionId(session),
        },
        Command::Session(SessionCommand::QueueEdit { session, id, text }) => {
            Request::EditQueuedMessage {
                session: SessionId(session),
                id,
                text,
            }
        }
        Command::Session(SessionCommand::QueueRemove { session, id }) => {
            Request::RemoveQueuedMessage {
                session: SessionId(session),
                id,
            }
        }
        Command::Session(SessionCommand::QueueMove { session, id, index }) => {
            Request::MoveQueuedMessage {
                session: SessionId(session),
                id,
                index,
            }
        }
        Command::Session(SessionCommand::QueueSendNow { session, id }) => {
            Request::SendQueuedMessageNow {
                session: SessionId(session),
                id,
            }
        }
        Command::Session(SessionCommand::Edit { session, seq, text }) => Request::EditPrompt {
            session: SessionId(session),
            seq,
            text,
        },
        Command::Session(SessionCommand::QueueAdd { session, text }) => Request::QueueMessage {
            session: SessionId(session),
            text,
        },
        Command::Session(SessionCommand::QueueInterrupt { session, id }) => {
            Request::InterruptWithQueuedMessage {
                session: SessionId(session),
                id,
            }
        }
        Command::Session(SessionCommand::QueuePause { session, resume }) => {
            Request::SetQueuePaused {
                session: SessionId(session),
                paused: !resume,
            }
        }
        Command::Session(SessionCommand::QueueClear { session }) => Request::ClearQueue {
            session: SessionId(session),
        },
        Command::Session(SessionCommand::Compact { session }) => Request::CompactSession {
            session: SessionId(session),
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
        Command::Session(SessionCommand::Cli { workspace }) => Request::CliSessions {
            workspace: WorkspaceId(workspace),
        },
        Command::Session(SessionCommand::Adopt {
            workspace,
            agent,
            id,
        }) => Request::AdoptCliSession {
            workspace: WorkspaceId(workspace),
            agent,
            vendor_session_id: id,
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
                amend: false,
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
        Response::PullRequest { url } => println!("{url}"),
        Response::Synced { pulled, pushed } => match (pulled, pushed) {
            (false, false) => println!("already in step with the remote"),
            (true, false) => println!("pulled"),
            (false, true) => println!("pushed"),
            (true, true) => println!("pulled and pushed"),
        },
        Response::Notes { notes } => {
            if notes.is_empty() {
                println!("{}", rust_i18n::t!("cli.notes.empty"));
            }
            for note in notes {
                println!(
                    "{}  {:<12} {}{}",
                    note.id,
                    note.project.map(|project| project.0).unwrap_or_default(),
                    note.title,
                    if note.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", note.tags.join(", "))
                    }
                );
            }
        }
        Response::Note { note } => println!("{}", note.id),
        Response::Tickets { tickets } => {
            if tickets.is_empty() {
                println!("{}", rust_i18n::t!("cli.tickets.empty"));
            }
            for ticket in tickets {
                println!(
                    "{}  {:<9} {:<24} {}",
                    ticket.id,
                    ticket.state.as_str(),
                    ticket.workspace.0,
                    ticket.title
                );
            }
        }
        Response::Ticket { ticket } => println!("{}", ticket.id),
        Response::QuickCommands { commands } => {
            for command in commands {
                println!(
                    "{}  {:<6} {:<12} {:<20} {}",
                    command.id,
                    command.kind.as_str(),
                    command
                        .project
                        .map(|project| project.0)
                        .unwrap_or_else(|| "*".into()),
                    command.name,
                    command.body.replace(['\r', '\n'], " ")
                );
            }
        }
        Response::QuickCommand { command } => println!("{}", command.id),
        Response::DaemonSettings { json } => println!("{json}"),
        Response::ProviderSettings { providers } => {
            for provider in providers {
                println!(
                    "{:<10} {:<8} {}",
                    provider.provider.as_str(),
                    if provider.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    provider.program.unwrap_or_default()
                );
            }
        }
        Response::BrowserSuggestions { pages } => {
            for page in pages {
                println!(
                    "{:>4}  {}  {}",
                    page.visits,
                    page.url,
                    page.title.unwrap_or_default()
                );
            }
        }
        Response::BundledSkillsInstalled { results } => {
            for result in results {
                println!("{:<10} {}", result.outcome, result.path);
            }
        }
        Response::CronJobs { jobs } => {
            for job in &jobs {
                print_cron_job(job);
            }
        }
        Response::CronJob { job } => print_cron_job(&job),
        Response::CronRuns { runs } => {
            for run in runs {
                println!(
                    "{}  {:<8} {}",
                    format_time(run.started_at),
                    run.outcome.as_str(),
                    run.detail.unwrap_or_default()
                );
            }
        }
        Response::Account { account } => print_accounts(std::slice::from_ref(&account), &[]),
        Response::PlanUsage { snapshot } => match snapshot {
            Some(snapshot) => print_plans(std::slice::from_ref(&snapshot)),
            None => println!("{}", rust_i18n::t!("cli.plan.unanswered")),
        },
        Response::Sessions { sessions } => print_sessions(&sessions),
        Response::SessionMatches { matches } => print_matches(&matches),
        Response::CliSessions { sessions } => print_cli_sessions(&sessions),
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
        Response::History { commits } => {
            for commit in commits {
                println!(
                    "{}\t{}\t{}\t{}",
                    commit.id.chars().take(8).collect::<String>(),
                    commit.authored_at,
                    commit.author,
                    commit.summary
                );
            }
        }
        Response::Files { files } => {
            for file in files {
                println!("{}", file.path);
            }
        }
        Response::Draft { text } => println!("{text}"),
        // The reference and nothing else, so it can be interpolated straight
        // into the next command.
        Response::Attachment { attachment } => println!("{}", attachment.reference),
        Response::AttachmentImage { image } => {
            if let Some(image) = image {
                println!("data:{};base64,{}", image.media_type, image.data_base64);
            }
        }
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
        Response::TerminalHistory { data } => {
            print!("{}", ginka_core::terminal::plain_text(&data))
        }
        Response::Usage {
            by_day,
            by_agent,
            by_account,
            plans,
            rates_fetched_at: _,
            by_model,
            by_project,
        } => {
            print_usage(&by_day, &by_agent, &by_account, &plans);
            print_usage_rows(&by_project);
            print_usage_rows(&by_model);
        }
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
        Response::Merged { outcome } => println!(
            "{}",
            if outcome.fast_forward {
                rust_i18n::t!(
                    "cli.merged.fast_forward",
                    into = &outcome.into,
                    commit = &outcome.commit[..outcome.commit.len().min(12)]
                )
            } else {
                rust_i18n::t!(
                    "cli.merged",
                    into = &outcome.into,
                    commit = &outcome.commit[..outcome.commit.len().min(12)]
                )
            }
        ),
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
        Response::QueuedMessages {
            messages,
            paused,
            resume_at,
            ..
        } => {
            match resume_at.and_then(|at| chrono::DateTime::from_timestamp(at, 0)) {
                Some(at) => println!(
                    "{}",
                    rust_i18n::t!(
                        "cli.queue.resumes_at",
                        at = at
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    )
                ),
                None if paused && !messages.is_empty() => {
                    println!("{}", rust_i18n::t!("cli.queue.paused"))
                }
                None => {}
            }
            for (index, message) in messages.into_iter().enumerate() {
                println!(
                    "{}\t{}\t{}",
                    index,
                    message.id,
                    message.text.replace(['\r', '\n'], " ")
                );
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
            "{:<24} {:<6} {}{}",
            project.name.0,
            project.kind.as_str(),
            project.path.display(),
            project
                .label
                .as_ref()
                .map(|label| format!("  [{label}]"))
                .unwrap_or_default()
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

/// Conversations the CLIs started, one per line: what `adopt` takes first.
fn print_cli_sessions(sessions: &[ginka_protocol::model::CliSession]) {
    if sessions.is_empty() {
        println!("{}", rust_i18n::t!("cli.session.cli.empty"));
        return;
    }
    for session in sessions {
        println!(
            "{:<7} {:<38} {:>4}  {}",
            session.agent, session.vendor_session_id, session.prompts, session.title
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
            match (row.totals.cost_usd, row.totals.unpriced) {
                // `≈` for a figure priced from the public table rather than
                // by the vendor (§3.3 N13).
                (Some(cost), 0) if row.totals.estimated => format!("≈${cost:.2}"),
                (Some(cost), 0) => format!("${cost:.2}"),
                (Some(cost), unpriced) => format!(
                    "{}${cost:.2} + {unpriced} unpriced",
                    if row.totals.estimated { "≈" } else { "" }
                ),
                (None, unpriced) if unpriced > 0 => format!("{unpriced} unpriced"),
                (None, _) => String::new(),
            }
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

/// A further section of `ginka usage`: a blank line, then a row per label.
fn print_usage_rows(rows: &[UsageRow]) {
    if rows.is_empty() {
        return;
    }
    println!();
    for row in rows {
        println!(
            "{:<24} {:>10} in {:>8} out {:>8} cached  {}",
            row.label,
            row.totals.input_tokens,
            row.totals.output_tokens,
            row.totals.cache_read_tokens,
            match (row.totals.cost_usd, row.totals.estimated) {
                (Some(cost), true) => format!("≈${cost:.2}"),
                (Some(cost), false) => format!("${cost:.2}"),
                (None, _) => String::new(),
            }
        );
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
            if account.active {
                format!("* {}", account.id.0)
            } else {
                account.id.0.clone()
            },
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
            // The id in brackets is what `session respond` answers.
            AgentEvent::AskUser {
                id,
                question,
                options,
            } => {
                if options.is_empty() {
                    format!("? {question} [{id}]")
                } else {
                    format!("? {question} [{id}] {}", options.join(" / "))
                }
            }
            AgentEvent::PlanProposal { id, plan } => format!("plan [{id}]: {plan}"),
            AgentEvent::Usage { usage } => format!(
                "usage: {} in, {} out",
                usage.input_tokens, usage.output_tokens
            ),
            AgentEvent::ContextUsage { usage } => format!(
                "context: {}/{} ({:.0}%)",
                usage.used_tokens,
                usage.window_tokens,
                usage.used_percent()
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
            AgentEvent::Permission { id, request } => format!("? permission [{id}]: {request}"),
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
            AgentEvent::Commands { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::SteerAccepted => String::new(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{ChangeSource, Cli, Request, request_for};
        use clap::Parser;

        #[test]
        fn changes_turn_selects_one_completed_checkpoint() {
            let cli = Cli::try_parse_from(["ginka", "changes", "w", "--turn", "c-1"]).unwrap();
            let request = request_for(cli.command).unwrap();
            assert!(matches!(
                request,
                Request::WorkspaceChanges {
                    source: ChangeSource::Turn { checkpoint },
                    ..
                } if checkpoint.0 == "c-1"
            ));
            assert!(
                Cli::try_parse_from(["ginka", "changes", "w", "--turn", "c-1", "--staged"])
                    .is_err()
            );
        }

        #[test]
        fn changes_context_is_bounded_and_sent_to_the_daemon() {
            let cli = Cli::try_parse_from(["ginka", "changes", "w", "--context", "10"]).unwrap();
            assert!(matches!(
                request_for(cli.command).unwrap(),
                Request::WorkspaceChanges {
                    context_lines: Some(10),
                    ..
                }
            ));
            assert!(Cli::try_parse_from(["ginka", "changes", "w", "--context", "26"]).is_err());
        }

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
        fn a_question_names_the_request_id_an_answer_is_sent_to() {
            let entry = |event| TranscriptEntry {
                seq: 9,
                at: 0,
                payload: TranscriptPayload::Agent { event },
            };
            let asked = transcript_line(&entry(AgentEvent::AskUser {
                id: "q-7".into(),
                question: "Which parser?".into(),
                options: vec!["serde".into(), "hand-written".into()],
            }));
            assert!(asked.contains("Which parser?"), "{asked}");
            assert!(asked.contains("[q-7]"), "{asked}");
            assert!(asked.contains("serde / hand-written"), "{asked}");
            let permission = transcript_line(&entry(AgentEvent::Permission {
                id: "p-1".into(),
                request: "run cargo test".into(),
            }));
            assert!(permission.contains("[p-1]"), "{permission}");
            let plan = transcript_line(&entry(AgentEvent::PlanProposal {
                id: "plan-2".into(),
                plan: "do it".into(),
            }));
            assert!(plan.contains("[plan-2]"), "{plan}");
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

/// One scheduled job on a line: id first, so a script can take it.
fn print_cron_job(job: &ginka_protocol::model::CronJob) {
    let next = match job.next_run_at {
        Some(at) => format_time(at),
        None => rust_i18n::t!("cli.cron.off").to_string(),
    };
    let last = job
        .last_run
        .as_ref()
        .map(|run| run.outcome.as_str())
        .unwrap_or("-");
    println!(
        "{}  {:<16} {:<8} {:<12} {:<20} next {}  last {}  {}",
        job.id,
        job.schedule,
        job.via.as_str(),
        job.project.0,
        job.name,
        next,
        last,
        job.body.replace(['\r', '\n'], " ")
    );
}

/// A Unix time as local wall-clock time.
fn format_time(at: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_opt(at, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| at.to_string())
}
