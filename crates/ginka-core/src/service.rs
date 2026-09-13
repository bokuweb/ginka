//! The daemon's behaviour, with no transport attached.
//!
//! [`Service`] answers every [`Request`] in the protocol. The daemon wraps it
//! in a WebSocket and the CLI reaches it over that socket, but nothing in here
//! knows about sockets, which is what makes the whole capability surface
//! testable without spawning a server.
//!
//! It owns the database connection and is not `Sync`: the daemon serialises
//! requests through it. That is deliberate — SQLite writes are serialised
//! anyway, and one owner means no half-applied mutation can be observed.

use crate::agent::Supervisor;
use crate::checkpoint;
use crate::connector::{self, ConnectorControl};
use crate::driver::{AgentDriver, Registry, SessionSpec};
use crate::registry;
use crate::{Paths, git, project, session};
use anyhow::Result;
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::ids::slugify;
use ginka_protocol::model::{
    AgentStatus, ChangeSource, Changes, Project, ProjectKind, Session, SessionOrigin, SessionState,
    WorkspaceSummary, Worktree,
};
use ginka_protocol::provider::{AccessMode, OptionOutcome, SessionOptions};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{CheckpointId, ProjectName, RpcError, SessionId, WorkspaceId};
use rusqlite::Connection;
use std::sync::{Arc, Mutex, MutexGuard};

/// Where the service announces things that happened.
///
/// Every mutation is pushed as well as answered, because a second window and
/// the CLI have to see a workspace appear without polling for it.
pub trait EventSink: Send + Sync + 'static {
    /// Deliver one event to every connected client. Must not block.
    fn emit(&self, event: DaemonEvent);
}

/// An event sink that drops everything, for a service with no clients.
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: DaemonEvent) {}
}

/// The daemon's request handler.
pub struct Service {
    paths: Paths,
    /// Shared with the supervisor's turn tasks, which append to transcripts
    /// while requests are being served. Every guard taken here is short-lived:
    /// nothing holds it across a git subprocess or a spawn.
    conn: Arc<Mutex<Connection>>,
    events: Arc<dyn EventSink>,
    drivers: Arc<Registry>,
    sessions: Supervisor,
    /// The shells running in this daemon. Owned here rather than by a window,
    /// so a build started in one keeps running when the window closes.
    terminals: crate::terminal::Terminals,
    /// What each workspace's git status was the last time it was polled.
    ///
    /// Kept so the poller can push only what changed: a client redrawing its
    /// sidebar every minute because nothing happened is a worse answer than a
    /// client that is told when something did.
    statuses: std::collections::HashMap<WorkspaceId, ginka_protocol::model::BranchStatus>,
    /// The last probe of the agent CLIs, and when it was taken.
    ///
    /// Probing runs two subprocesses per agent, and the sidebar asks on every
    /// tick; an installed CLI does not come and go between them.
    agents: Option<(std::time::Instant, Vec<AgentStatus>)>,
    /// The daemon's settings as loaded at start and changed by requests
    /// since. The accounts live here (`docs/accounts.md` §3), so a change is
    /// written back to the file.
    settings: crate::settings::DaemonSettings,
    /// The last probe of every account's sign-in, and when it was taken.
    accounts: Option<(std::time::Instant, Vec<ginka_protocol::model::Account>)>,
    /// Set when a sign-in terminal closed: whatever the vendor said before,
    /// the next `Accounts` asks again.
    accounts_stale: Arc<std::sync::atomic::AtomicBool>,
    /// The chat connectors the daemon registered, for `ListConnectors` and
    /// `TestConnector`. The service knows what they are called and nothing
    /// about how they connect (`docs/connectors.md` §2).
    connectors: Vec<Arc<dyn ConnectorControl>>,
    /// Where the `ginka` command is, for the MCP bridge every agent is
    /// handed (`crate::tools`). Looked up once: it does not move while the
    /// daemon runs, and the log says once when it is missing.
    cli: Option<std::path::PathBuf>,
}

/// How long a probe of the agent CLIs is trusted for.
///
/// Long enough that a window ticking every fifteen seconds does not shell out
/// each time, short enough that signing in is noticed while the user is still
/// wondering why it said they had not.
const AGENT_PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

impl Service {
    /// Build a service around an already-open database.
    pub fn new(paths: Paths, conn: Connection, events: Arc<dyn EventSink>) -> Self {
        let conn = Arc::new(Mutex::new(conn));
        // Read here rather than passed in: how many checkpoints a workspace
        // keeps is the user's setting, and a service built without one still
        // has to prune.
        let settings: crate::settings::DaemonSettings =
            crate::settings::load(&paths.daemon_settings());
        let cli = crate::tools::locate_cli();
        if cli.is_none() {
            tracing::warn!(
                "no `ginka` command found beside the daemon or on PATH; agents are started \
                 without Ginka's MCP bridge (set {} to name one)",
                crate::tools::CLI_ENV
            );
        }
        Self {
            cli,
            sessions: Supervisor::new(conn.clone(), events.clone(), settings.checkpoint_limit),
            paths,
            terminals: crate::terminal::Terminals::new(events.clone()),
            conn,
            events,
            drivers: Arc::new(Registry::with_defaults()),
            statuses: std::collections::HashMap::new(),
            agents: None,
            settings,
            accounts: None,
            accounts_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            connectors: Vec::new(),
        }
    }

    /// Let a hosted connector be asked about and tested through the
    /// protocol. Registering the same id twice replaces the first.
    pub fn register_connector(&mut self, connector: Arc<dyn ConnectorControl>) {
        self.connectors
            .retain(|existing| existing.id() != connector.id());
        self.connectors.push(connector);
    }

    /// The daemon's settings as the service currently holds them.
    pub fn settings(&self) -> &crate::settings::DaemonSettings {
        &self.settings
    }

    /// Use these drivers rather than the ones this build ships.
    ///
    /// This is how the tests put a scripted agent behind the `claude` id, and
    /// how a user's configured binaries will be installed later.
    pub fn with_drivers(mut self, drivers: Registry) -> Self {
        self.drivers = Arc::new(drivers);
        self
    }

    /// Hand agents this `ginka` command as their MCP bridge, rather than the
    /// one found beside the daemon. How a test says where the CLI is.
    pub fn with_cli(mut self, cli: impl Into<std::path::PathBuf>) -> Self {
        self.cli = Some(cli.into());
        self
    }

    /// The MCP servers an agent starting in `worktree` is told about.
    fn mcp_servers(&self, worktree: &std::path::Path) -> Vec<crate::tools::McpServer> {
        let zg = self
            .settings
            .tools
            .zvec_grep
            .then(|| crate::tools::on_path("zg"))
            .flatten();
        crate::tools::servers_for(
            &self.settings.tools,
            &self.paths,
            worktree,
            self.cli.as_deref(),
            zg.as_deref(),
        )
    }

    /// Open the database `paths` points at, running migrations, and build a
    /// service around it.
    ///
    /// Sessions the previous daemon was running are marked failed here: their
    /// processes died with it, and leaving them looking like working agents is
    /// a lie the user would act on.
    pub fn open(paths: Paths, events: Arc<dyn EventSink>) -> Result<Self> {
        paths.ensure()?;
        let conn = crate::db::open(&paths.database())?;
        let orphans = session::mark_orphans_failed(&conn, now())?;
        if orphans > 0 {
            tracing::warn!(orphans, "sessions did not survive the previous daemon");
        }
        // The retention sweep (`docs/roadmap.md` §4.4): cost history is
        // interesting for a month and clutter forever.
        let settings: crate::settings::DaemonSettings =
            crate::settings::load(&paths.daemon_settings());
        match crate::usage::sweep(&conn, settings.retention_days) {
            Ok(swept) if swept > 0 => tracing::info!(swept, "swept old usage"),
            Err(error) => tracing::warn!(%error, "could not sweep old usage"),
            _ => {}
        }
        Ok(Self::new(paths, conn, events))
    }

    /// Where this service keeps its state.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The database, locked for as long as the caller holds the guard.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Answer one request.
    ///
    /// Errors are protocol errors rather than `anyhow`: they cross a process
    /// boundary, so a client has to be able to match on `code` instead of
    /// reading a sentence.
    pub fn handle(&mut self, request: Request) -> Result<Response, RpcError> {
        match request {
            Request::Ping => Ok(Response::Ack),

            Request::ListProjects => Ok(Response::Projects {
                projects: self.projects()?,
            }),
            Request::AddProject { path, label } => {
                let project = registry::register_project_as(&self.conn(), &path, label.as_deref())
                    .map_err(failed)?;
                registry::sync_worktrees(&self.conn(), &project).map_err(failed)?;
                self.events.emit(DaemonEvent::ProjectsChanged);
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name.clone(),
                });
                Ok(Response::Project { project })
            }
            Request::RemoveProject { project } => {
                if !project::remove_project(&self.conn(), &project).map_err(failed)? {
                    return Err(self.no_such_project(&project));
                }
                self.events.emit(DaemonEvent::ProjectsChanged);
                Ok(Response::Ack)
            }

            Request::ListWorkspaces { project } => {
                let projects = match project {
                    Some(name) => vec![self.project(&name)?],
                    None => self.projects()?,
                };
                let mut workspaces = Vec::new();
                for project in &projects {
                    registry::sync_worktrees(&self.conn(), project).map_err(failed)?;
                    // Bound to a local first: a guard taken in a `for`'s
                    // iterator expression lives for the whole loop body, and
                    // `summarize` needs the connection too.
                    let worktrees =
                        project::list_worktrees(&self.conn(), &project.name).map_err(failed)?;
                    for worktree in worktrees {
                        workspaces.push(self.summarize(worktree));
                    }
                }
                Ok(Response::Workspaces { workspaces })
            }
            Request::CreateWorkspace {
                project,
                branch,
                base,
            } => {
                let project = self.project(&project)?;
                if project.kind == ProjectKind::Plain {
                    return Err(RpcError::failed(format!(
                        "{} is a plain folder and has no worktrees",
                        project.name
                    )));
                }
                let base = base.unwrap_or_else(|| project.default_branch.clone());
                // Worktrees live under Ginka's own directory rather than beside
                // the user's checkout, so the app never litters the repository
                // it was pointed at.
                let path = self
                    .paths
                    .worktrees()
                    .join(&project.name.0)
                    .join(slugify(&branch));
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(failed)?;
                }
                git::add_worktree(&project.path, &path, &branch, &base).map_err(failed)?;
                // git records the resolved path, which on macOS differs from
                // the one we asked for (/var against /private/var).
                let path = path.canonicalize().unwrap_or(path);
                self.set_up(&project.path, &path);
                registry::sync_worktrees(&self.conn(), &project).map_err(failed)?;
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name.clone(),
                });

                let worktree = project::list_worktrees(&self.conn(), &project.name)
                    .map_err(failed)?
                    .into_iter()
                    .find(|worktree| worktree.path == path || worktree.branch == branch)
                    .ok_or_else(|| {
                        RpcError::failed(format!(
                            "created {} but git did not report it as a worktree",
                            path.display()
                        ))
                    })?;
                Ok(Response::Workspace {
                    workspace: self.summarize(worktree),
                })
            }
            Request::CreateScratchWorkspace { name } => {
                let project = registry::create_scratch_workspace(
                    &self.conn(),
                    &self.paths,
                    name.as_deref(),
                    &today(),
                )
                .map_err(failed)?;
                self.events.emit(DaemonEvent::ProjectsChanged);
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name.clone(),
                });

                let worktree = project::list_worktrees(&self.conn(), &project.name)
                    .map_err(failed)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        RpcError::failed("the scratch directory was made but not registered")
                    })?;
                Ok(Response::Workspace {
                    workspace: self.summarize(worktree),
                })
            }
            Request::RemoveWorkspace { workspace, force } => {
                let worktree = self.worktree(&workspace)?;
                let project = self.project(&worktree.project)?;
                git::remove_worktree(&project.path, &worktree.path, force).map_err(failed)?;
                registry::sync_worktrees(&self.conn(), &project).map_err(failed)?;
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name,
                });
                Ok(Response::Ack)
            }
            Request::PinWorkspace { workspace, pinned } => {
                if !project::set_pinned(&self.conn(), &workspace, pinned).map_err(failed)? {
                    return Err(RpcError::not_found(format!(
                        "no workspace named {workspace}"
                    )));
                }
                if let Some((project, _)) = workspace.parts() {
                    self.events.emit(DaemonEvent::WorkspacesChanged { project });
                }
                Ok(Response::Ack)
            }
            Request::ArchiveWorkspace {
                workspace,
                archived,
            } => {
                if !project::set_archived(&self.conn(), &workspace, archived).map_err(failed)? {
                    return Err(RpcError::not_found(format!(
                        "no workspace named {workspace}"
                    )));
                }
                if let Some((project, _)) = workspace.parts() {
                    self.events.emit(DaemonEvent::WorkspacesChanged { project });
                }
                Ok(Response::Ack)
            }

            Request::ListAgents => Ok(Response::Agents {
                agents: self.agents(),
            }),
            Request::Accounts => Ok(Response::Accounts {
                accounts: self.accounts(),
            }),
            Request::AddAccount {
                id,
                provider,
                label,
            } => {
                let driver = self.drivers.get(provider.as_str()).ok_or_else(|| {
                    account_error(crate::account::AccountError::NoDriver(provider))
                })?;
                crate::account::add(
                    &mut self.settings,
                    &self.paths,
                    id.clone(),
                    provider,
                    label,
                    driver.home_variable(),
                )
                .map_err(account_error)?;
                self.save_settings()?;
                self.accounts = None;
                self.events.emit(DaemonEvent::AccountsChanged);
                // Listed rather than probed: the directory is empty until the
                // user signs in, and asking the vendor about it now would only
                // say so slowly.
                let account = crate::account::list(&self.settings, &self.paths, &self.drivers)
                    .into_iter()
                    .find(|account| account.id == id)
                    .ok_or_else(|| RpcError::failed("the account was added but is not listed"))?;
                Ok(Response::Account { account })
            }
            Request::RemoveAccount { id, delete_home } => {
                crate::account::remove(&mut self.settings, &self.paths, &id, delete_home)
                    .map_err(account_error)?;
                self.save_settings()?;
                self.accounts = None;
                self.events.emit(DaemonEvent::AccountsChanged);
                Ok(Response::Ack)
            }
            Request::LoginAccount {
                id,
                workspace,
                rows,
                cols,
            } => self.login_account(&id, &workspace, rows, cols),
            Request::RefreshPlanUsage { account } => self.refresh_plan_usage(&account),
            Request::ListSessions { workspace, origin } => Ok(Response::Sessions {
                sessions: match origin {
                    Some(origin) => session::find_by_origin(&self.conn(), &origin)
                        .map_err(failed)?
                        .into_iter()
                        .filter(|session| {
                            workspace
                                .as_ref()
                                .is_none_or(|wanted| &session.workspace == wanted)
                        })
                        .collect(),
                    None => session::list(&self.conn(), workspace.as_ref()).map_err(failed)?,
                },
            }),
            Request::StartSession {
                workspace,
                agent,
                prompt,
                model,
                reasoning_effort,
                service_tier,
                account,
                access_mode,
                origin,
            } => self.start_session(
                workspace,
                &agent,
                prompt,
                model,
                reasoning_effort,
                service_tier,
                account,
                access_mode.unwrap_or_default(),
                origin,
            ),
            Request::UpdateSessionOptions {
                session,
                model,
                reasoning_effort,
                service_tier,
            } => self.update_session_options(&session, model, reasoning_effort, service_tier),
            Request::CloseSessionOrigin { session } => {
                self.session(&session)?;
                if !session::close_origin(&self.conn(), &session).map_err(failed)? {
                    return Err(RpcError::failed(format!(
                        "session {session} did not come from a chat platform"
                    )));
                }
                Ok(Response::Ack)
            }
            Request::FanOut {
                project,
                branch_prefix,
                base,
                prompt,
                attempts,
            } => self.fan_out(project, &branch_prefix, base, &prompt, &attempts),
            Request::SendMessage { session, text } => self.send_message(&session, text),
            Request::RespondToAgent {
                session,
                request_id,
                response,
            } => {
                self.session(&session)?;
                self.sessions
                    .respond(&session, &request_id, &response)
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::RenameSession { session, title } => {
                if !session::rename(&self.conn(), &session, &title).map_err(failed)? {
                    return Err(RpcError::not_found(format!("no session with id {session}")));
                }
                Ok(Response::Ack)
            }
            Request::RemoveSession { session } => {
                let stored = self.session(&session)?;
                // Whatever is running for it has to go first, or the daemon
                // keeps feeding a transcript that no longer exists.
                self.sessions.cancel(&session);
                if !session::remove(&self.conn(), &session).map_err(failed)? {
                    return Err(RpcError::not_found(format!("no session with id {session}")));
                }
                self.events.emit(DaemonEvent::SessionStateChanged {
                    session,
                    state: SessionState::Cancelled,
                });
                if let Some((project, _)) = stored.workspace.parts() {
                    self.events.emit(DaemonEvent::WorkspacesChanged { project });
                }
                Ok(Response::Ack)
            }
            Request::ForkSession {
                session,
                after,
                agent,
                model,
                account,
            } => self.fork_session(&session, after, agent.as_deref(), model, account),
            Request::SearchSessions {
                workspace,
                query,
                limit,
            } => Ok(Response::SessionMatches {
                matches: session::search(
                    &self.conn(),
                    workspace.as_ref(),
                    &query,
                    limit.unwrap_or(50),
                )
                .map_err(failed)?,
            }),
            Request::CancelSession { session } => {
                self.session(&session)?;
                self.sessions.cancel(&session);
                Ok(Response::Ack)
            }
            Request::SessionTranscript {
                session,
                after,
                limit,
            } => {
                self.session(&session)?;
                Ok(Response::Transcript {
                    entries: session::transcript(&self.conn(), &session, after, limit)
                        .map_err(failed)?,
                })
            }

            Request::WorkspaceChanges { workspace, source } => {
                let worktree = self.worktree(&workspace)?;
                let files = match &source {
                    // A checkpoint names a commit, and the commit is what git
                    // can be asked about.
                    ChangeSource::SinceCheckpoint { checkpoint } => {
                        let stored = checkpoint::get(&self.conn(), checkpoint)
                            .map_err(failed)?
                            .ok_or_else(|| {
                                RpcError::not_found(format!(
                                    "no checkpoint with id {}",
                                    checkpoint.0
                                ))
                            })?;
                        git::changes_since(&worktree.path, &stored.commit).map_err(failed)?
                    }
                    other => git::changes(&worktree.path, other).map_err(failed)?,
                };
                Ok(Response::Changes {
                    changes: Changes { source, files },
                })
            }
            Request::Commit {
                workspace,
                message,
                all,
            } => {
                let worktree = self.worktree(&workspace)?;
                if message.trim().is_empty() {
                    return Err(RpcError::failed("a commit needs a message"));
                }
                let commit = git::commit(&worktree.path, &message, all).map_err(failed)?;
                // The branch moved, so what the sidebar says about it is stale.
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: worktree.project.clone(),
                });
                Ok(Response::Committed { commit })
            }
            Request::StageFile {
                workspace,
                path,
                staged,
            } => {
                let worktree = self.worktree(&workspace)?;
                if staged {
                    git::stage(&worktree.path, &path).map_err(failed)?;
                } else {
                    git::unstage(&worktree.path, &path).map_err(failed)?;
                }
                Ok(Response::Ack)
            }
            Request::RevertFile { workspace, path } => {
                let worktree = self.worktree(&workspace)?;
                git::revert_file(&worktree.path, &path).map_err(failed)?;
                // The file is back to what it was, so the sidebar's count of
                // what is uncommitted is stale.
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: worktree.project.clone(),
                });
                Ok(Response::Ack)
            }
            Request::Push { workspace } => {
                let worktree = self.worktree(&workspace)?;
                git::push(&worktree.path).map_err(failed)?;
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: worktree.project.clone(),
                });
                Ok(Response::Ack)
            }
            Request::WorkspaceFiles {
                workspace,
                query,
                limit,
            } => {
                let worktree = self.worktree(&workspace)?;
                let paths = crate::files::list(&worktree.path).map_err(failed)?;
                Ok(Response::Files {
                    files: crate::files::search(
                        &paths,
                        query.as_deref().unwrap_or_default(),
                        limit
                            .map(|limit| limit as usize)
                            .unwrap_or(crate::files::DEFAULT_LIMIT),
                    ),
                })
            }
            Request::SlashCommands { workspace, query } => {
                let worktree = self.worktree(&workspace)?;
                // The user's own commands live in their home, which is theirs
                // rather than Ginka's `GINKA_HOME`.
                let found = crate::commands::discover(&worktree.path, dirs::home_dir().as_deref());
                Ok(Response::Commands {
                    commands: crate::commands::search(&found, query.as_deref().unwrap_or_default()),
                })
            }
            Request::IndexWorkspace {
                workspace,
                rows,
                cols,
            } => {
                let worktree = self.worktree(&workspace)?;
                let zg = crate::tools::on_path("zg").ok_or_else(|| {
                    RpcError::failed(
                        "zg is not installed; `npm install -g @zvec/zvec-grep` puts it on PATH",
                    )
                })?;
                let terminal = self
                    .terminals
                    .open_command(
                        &workspace,
                        &worktree.path,
                        crate::terminal::TerminalCommand {
                            program: zg.to_string_lossy().into_owned(),
                            args: vec!["index".to_string()],
                            env: Vec::new(),
                            title: "zg index".to_string(),
                            on_exit: None,
                        },
                        rows,
                        cols,
                    )
                    .map_err(failed)?;
                Ok(Response::Terminal { terminal })
            }
            Request::GenerateCommitMessage {
                workspace,
                agent,
                staged,
            } => self.generate_commit_message(workspace, agent.as_deref(), staged),
            Request::ListBranches { workspace } => {
                let worktree = self.worktree(&workspace)?;
                Ok(Response::Branches {
                    branches: git::list_branches(&worktree.path).map_err(failed)?,
                })
            }
            Request::CheckoutBranch {
                workspace,
                branch,
                create,
            } => {
                let worktree = self.worktree(&workspace)?;
                git::checkout(&worktree.path, &branch, create).map_err(failed)?;
                // The workspace keeps its id; only its live branch moved, and
                // the next listing reconciles that against git.
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: worktree.project.clone(),
                });
                Ok(Response::Ack)
            }
            Request::ListSkills { project } => {
                let catalog = self.skills(project.as_ref())?;
                Ok(Response::Skills {
                    skills: catalog.skills,
                    truncated: catalog.truncated,
                })
            }
            Request::SetSkillEnabled {
                name,
                enabled,
                project,
            } => {
                let catalog = self.skills(project.as_ref())?;
                let skill = catalog
                    .skills
                    .iter()
                    .find(|skill| skill.name == name)
                    .ok_or_else(|| RpcError::not_found(format!("no skill named {name}")))?;
                crate::skills::set_enabled(skill, enabled).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::ComposerDraft { workspace } => Ok(Response::Draft {
                text: session::draft(&self.conn(), &workspace).map_err(failed)?,
            }),
            Request::SaveComposerDraft { workspace, text } => {
                session::set_draft(&self.conn(), &workspace, &text, now()).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::Usage { days } => {
                let days = days.unwrap_or(30);
                let conn = self.conn();
                Ok(Response::Usage {
                    by_day: crate::usage::by_day(&conn, days).map_err(failed)?,
                    by_agent: crate::usage::by_agent(&conn, days).map_err(failed)?,
                    by_account: crate::usage::by_account(&conn, days).map_err(failed)?,
                    plans: crate::usage::plans(&conn).map_err(failed)?,
                })
            }
            Request::AddReviewComment {
                workspace,
                path,
                line,
                side,
                text,
            } => {
                self.worktree(&workspace)?;
                if text.trim().is_empty() {
                    return Err(RpcError::failed(
                        "a comment with nothing in it says nothing",
                    ));
                }
                crate::comments::add(&self.conn(), &workspace, &path, line, side, &text, now())
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::ListReviewComments { workspace } => Ok(Response::ReviewComments {
                comments: crate::comments::list(&self.conn(), &workspace).map_err(failed)?,
            }),
            Request::RemoveReviewComment { comment } => {
                if !crate::comments::remove(&self.conn(), &comment).map_err(failed)? {
                    return Err(RpcError::not_found(format!("no comment with id {comment}")));
                }
                Ok(Response::Ack)
            }
            Request::SendReviewComments { workspace, session } => {
                let comments = crate::comments::list(&self.conn(), &workspace).map_err(failed)?;
                if comments.is_empty() {
                    return Err(RpcError::failed("there are no comments to send"));
                }
                let message = crate::comments::compose(&comments);
                self.send_message(&session, message)?;
                // Cleared only once the agent has them: a batch that vanished
                // into a failed send would be a review done twice.
                crate::comments::clear(&self.conn(), &workspace).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::ListCheckpoints { workspace } => {
                self.worktree(&workspace)?;
                Ok(Response::Checkpoints {
                    checkpoints: checkpoint::list(&self.conn(), &workspace).map_err(failed)?,
                })
            }
            Request::RestoreCheckpoint { checkpoint } => self.restore(&checkpoint),

            Request::UploadAttachment { name, data_base64 } => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data_base64.trim())
                    .map_err(|error| {
                        RpcError::malformed(format!("attachment is not valid base64: {error}"))
                    })?;
                let stored = crate::attachment::AttachmentStore::new(self.paths.attachments())
                    .put(&name, &bytes)
                    .map_err(failed)?;
                Ok(Response::Attachment {
                    attachment: ginka_protocol::model::Attachment {
                        reference: stored.reference,
                        name: stored.name,
                        bytes: stored.bytes,
                    },
                })
            }

            Request::OpenTerminal {
                workspace,
                rows,
                cols,
            } => {
                let worktree = self.worktree(&workspace)?;
                let terminal = self
                    .terminals
                    .open(&workspace, &worktree.path, rows, cols)
                    .map_err(failed)?;
                Ok(Response::Terminal { terminal })
            }
            Request::SearchContent {
                workspace,
                query,
                limit,
            } => {
                let worktree = self.worktree(&workspace)?;
                let limit = limit
                    .map(|limit| limit as usize)
                    .unwrap_or(crate::files::DEFAULT_LIMIT);
                Ok(Response::Matches {
                    matches: crate::files::search_content(&worktree.path, &query, limit)
                        .map_err(failed)?,
                })
            }
            Request::ReadFile { workspace, path } => {
                let worktree = self.worktree(&workspace)?;
                Ok(Response::FileContent {
                    file: crate::files::read(&worktree.path, &path).map_err(failed)?,
                })
            }
            Request::WriteFile {
                workspace,
                path,
                text,
                expected_revision,
            } => {
                let worktree = self.worktree(&workspace)?;
                Ok(Response::FileContent {
                    file: crate::files::write(&worktree.path, &path, &text, &expected_revision)
                        .map_err(failed)?,
                })
            }
            Request::WorkspaceTerminals { workspace } => Ok(Response::Terminals {
                terminals: self.terminals.list(&workspace),
            }),
            Request::TerminalHistory { terminal } => Ok(Response::TerminalHistory {
                // A terminal that has closed is one the client is asking about
                // because it has not heard yet, which is a `not found` rather
                // than a failure.
                data: self
                    .terminals
                    .history(&terminal)
                    .map_err(|error| RpcError::not_found(error.to_string()))?,
            }),
            Request::WriteTerminal { terminal, data } => {
                self.terminals.write(&terminal, &data).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::ResizeTerminal {
                terminal,
                rows,
                cols,
            } => {
                self.terminals
                    .resize(&terminal, rows, cols)
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::CloseTerminal { terminal } => {
                self.terminals.close(&terminal).map_err(failed)?;
                Ok(Response::Ack)
            }

            Request::ListConnectors => Ok(Response::Connectors {
                connectors: self.connector_states(),
            }),
            Request::AllowConnectorSender { connector, sender } => {
                if connector != connector::SLACK {
                    return Err(RpcError::not_found(format!(
                        "no connector named {connector}; this build has: {}",
                        connector::SLACK
                    )));
                }
                let sender = sender.trim().to_string();
                if sender.is_empty() {
                    return Err(RpcError::failed("a sender id is needed"));
                }
                let slack = self.settings.connectors.slack.get_or_insert_default();
                if !slack.allowed_users.contains(&sender) {
                    slack.allowed_users.push(sender);
                }
                self.save_settings()?;
                for hosted in &self.connectors {
                    hosted.reload(&self.settings.connectors);
                    self.events.emit(DaemonEvent::ConnectorStateChanged {
                        state: hosted.state(),
                    });
                }
                Ok(Response::Ack)
            }
            Request::TestConnector { connector, channel } => {
                let hosted = self
                    .connectors
                    .iter()
                    .find(|hosted| hosted.id() == connector)
                    .ok_or_else(|| {
                        RpcError::not_found(format!("connector {connector} is not running"))
                    })?;
                hosted.test(&channel).map_err(failed)?;
                Ok(Response::Ack)
            }

            Request::Shutdown => {
                // The transport is listening for this: it is the one event a
                // client cannot poll for after the fact.
                self.events.emit(DaemonEvent::Shutdown);
                Ok(Response::Ack)
            }
        }
    }

    /// Every connector this build knows, running or not.
    fn connector_states(&self) -> Vec<ginka_protocol::model::ConnectorState> {
        let mut states: Vec<ginka_protocol::model::ConnectorState> = self
            .connectors
            .iter()
            .map(|hosted| hosted.state())
            .collect();
        if !states.iter().any(|state| state.id == connector::SLACK) {
            states.push(connector::unconfigured_state(
                connector::SLACK,
                &self.settings.connectors,
            ));
        }
        states
    }

    /// What each agent CLI says about itself, from the last probe or a new one.
    fn agents(&mut self) -> Vec<AgentStatus> {
        let fresh = self
            .agents
            .as_ref()
            .is_some_and(|(taken, _)| taken.elapsed() < AGENT_PROBE_TTL);
        if !fresh {
            let probed = crate::driver::probe::probe_all(&self.drivers);
            self.agents = Some((std::time::Instant::now(), probed));
        }
        self.agents
            .as_ref()
            .map(|(_, agents)| agents.clone())
            .unwrap_or_default()
    }

    /// Every account, with what the vendor last said about each being signed
    /// in — from the last probe, or a new one.
    fn accounts(&mut self) -> Vec<ginka_protocol::model::Account> {
        let stale = self
            .accounts_stale
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        let fresh = !stale
            && self
                .accounts
                .as_ref()
                .is_some_and(|(taken, _)| taken.elapsed() < AGENT_PROBE_TTL);
        if !fresh {
            let mut accounts = crate::account::list(&self.settings, &self.paths, &self.drivers);
            for account in &mut accounts {
                let Some(driver) = self.drivers.get(account.provider.as_str()) else {
                    continue;
                };
                let env = crate::account::env_layer(
                    &self.settings,
                    &self.paths,
                    &account.id,
                    driver.home_variable(),
                );
                account.signed_in = crate::driver::probe::probe_signed_in(driver.as_ref(), &env);
            }
            self.accounts = Some((std::time::Instant::now(), accounts));
        }
        self.accounts
            .as_ref()
            .map(|(_, accounts)| accounts.clone())
            .unwrap_or_default()
    }

    /// The driver an account runs on, and the environment that points the
    /// driver's CLI at the account (`docs/accounts.md` §4).
    fn account_env(&self, account: &ginka_protocol::AccountId) -> Result<AccountRuntime, RpcError> {
        let provider =
            crate::account::provider_of(&self.settings, account).map_err(account_error)?;
        let driver = self.driver(&provider)?;
        let env =
            crate::account::env_layer(&self.settings, &self.paths, account, driver.home_variable());
        Ok((driver, env))
    }

    /// Run the vendor's own sign-in for an account, in a terminal in the
    /// workspace's dock.
    ///
    /// Ginka performs no login: the browser round-trip and the token are the
    /// vendor's, written into the account's directory. When the command
    /// exits, the next `Accounts` asks the vendor again.
    fn login_account(
        &mut self,
        id: &ginka_protocol::AccountId,
        workspace: &WorkspaceId,
        rows: u16,
        cols: u16,
    ) -> Result<Response, RpcError> {
        let worktree = self.worktree(workspace)?;
        let (driver, _) = self.account_env(id)?;
        let command = driver.login_command().ok_or_else(|| {
            RpcError::failed(format!(
                "{} has no sign-in command; sign in with the CLI itself",
                driver.display_name()
            ))
        })?;
        // The directory and nothing else: a login does not need the account's
        // own variables, and one of those may be a key.
        let env = driver
            .home_variable()
            .filter(|_| !id.is_default())
            .map(|variable| {
                vec![(
                    variable.to_string(),
                    crate::account::home_dir(&self.paths, id)
                        .to_string_lossy()
                        .into_owned(),
                )]
            })
            .unwrap_or_default();
        let label = self
            .settings
            .accounts
            .get(&id.0)
            .map(|account| account.label.clone())
            .unwrap_or_else(|| driver.display_name().to_string());
        let stale = self.accounts_stale.clone();
        let events = self.events.clone();
        let terminal = self
            .terminals
            .open_command(
                workspace,
                &worktree.path,
                crate::terminal::TerminalCommand {
                    program: command.program,
                    args: command.args,
                    env,
                    title: format!("sign in: {label}"),
                    on_exit: Some(Box::new(move || {
                        stale.store(true, std::sync::atomic::Ordering::SeqCst);
                        events.emit(DaemonEvent::AccountsChanged);
                    })),
                },
                rows,
                cols,
            )
            .map_err(failed)?;
        Ok(Response::Terminal { terminal })
    }

    /// Ask the provider for an account's rate-limit windows now, and keep the
    /// answer (`docs/accounts.md` §6).
    fn refresh_plan_usage(
        &mut self,
        account: &ginka_protocol::AccountId,
    ) -> Result<Response, RpcError> {
        let (driver, env) = self.account_env(account)?;
        let Some(usage) = crate::driver::probe::probe_plan_usage(driver.as_ref(), &env) else {
            return Ok(Response::PlanUsage { snapshot: None });
        };
        let snapshot = crate::usage::record_plan(
            &self.conn(),
            account,
            &usage,
            now(),
            ginka_protocol::model::PlanSource::Fetched,
        )
        .map_err(failed)?;
        self.events.emit(DaemonEvent::PlanUsageChanged {
            snapshot: snapshot.clone(),
        });
        Ok(Response::PlanUsage {
            snapshot: Some(snapshot),
        })
    }

    /// Write the settings back, so a change made through a request survives
    /// the daemon.
    fn save_settings(&self) -> Result<(), RpcError> {
        crate::settings::save(&self.paths.daemon_settings(), &self.settings).map_err(failed)
    }

    /// Start an agent in a workspace.
    #[allow(clippy::too_many_arguments)]
    fn start_session(
        &mut self,
        workspace: WorkspaceId,
        agent: &str,
        prompt: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
        account: Option<ginka_protocol::AccountId>,
        access_mode: AccessMode,
        origin: Option<SessionOrigin>,
    ) -> Result<Response, RpcError> {
        let worktree = self.worktree(&workspace)?;
        let driver = self.driver(agent)?;
        let account = crate::account::resolve(&self.settings, driver.id(), account.as_ref())
            .map_err(account_error)?;
        if let Some(origin) = &origin
            && let Some(existing) = session::find_by_origin(&self.conn(), origin).map_err(failed)?
        {
            return Err(RpcError::failed(format!(
                "session {} is already answering that thread; send to it, or close its origin first",
                existing.id
            )));
        }
        let now = now();
        let session = Session {
            id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
            workspace,
            agent: driver.id().to_string(),
            account: account.clone(),
            model: model.clone(),
            reasoning_effort: reasoning_effort.clone(),
            service_tier: service_tier.clone(),
            state: SessionState::Starting,
            // What this conversation is about, taken from what was asked. The
            // user can rename it; nothing else writes it, because the agent's
            // own summary is a different thing and changes every turn.
            title: Some(title_from(&prompt)),
            summary: None,
            vendor_session_id: None,
            access_mode,
            origin,
            created_at: now,
            updated_at: now,
        };
        session::insert(&self.conn(), &session).map_err(failed)?;
        self.events.emit(DaemonEvent::SessionStarted {
            session: Box::new(session.clone()),
        });

        // The agent reads files, not URIs: an attachment the user mentioned
        // reaches it as a path on this host (§3.3 N6). The transcript keeps
        // the reference, so the window can still draw the attachment.
        let mut spec = SessionSpec::new(&worktree.path, self.expand_attachments(&prompt))
            .with_model(model)
            .with_reasoning_effort(reasoning_effort)
            .with_service_tier(service_tier)
            .with_access_mode(access_mode)
            .with_mcp_servers(self.mcp_servers(&worktree.path));
        for (key, value) in crate::account::env_layer(
            &self.settings,
            &self.paths,
            &account,
            driver.home_variable(),
        ) {
            spec = spec.with_env(key, value);
        }
        self.sessions
            .start(session.id.clone(), driver, spec)
            .map_err(failed)?;
        Ok(Response::Session { session })
    }

    /// Apply provider options to later turns without losing the vendor thread
    /// when its transport can carry them on resume.
    fn update_session_options(
        &mut self,
        id: &SessionId,
        model: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    ) -> Result<Response, RpcError> {
        let mut stored = self.session(id)?;
        let before = SessionOptions {
            model: stored.model.clone(),
            reasoning_effort: stored.reasoning_effort.clone(),
            service_tier: stored.service_tier.clone(),
            access_mode: stored.access_mode,
            account: Some(stored.account.clone()),
        };
        let after = SessionOptions {
            model: model.clone(),
            reasoning_effort: reasoning_effort.clone(),
            service_tier: service_tier.clone(),
            ..before.clone()
        };
        if before == after {
            return Ok(Response::SessionOptionsApplied {
                session: stored,
                outcome: OptionOutcome::Absorbed,
            });
        }

        let driver = self.driver(&stored.agent)?;
        let outcome = if SessionOptions::forces_restart(&before, &after) {
            OptionOutcome::RestartRequired
        } else {
            driver.apply_options(&before, &after)
        };
        if outcome.absorbed() {
            let updated_at = now();
            let changed = session::update_provider_options(
                &self.conn(),
                id,
                model.as_deref(),
                reasoning_effort.as_deref(),
                service_tier.as_deref(),
                updated_at,
            )
            .map_err(failed)?;
            if !changed {
                return Err(RpcError::not_found(format!("session {id}")));
            }
            stored.model = model;
            stored.reasoning_effort = reasoning_effort;
            stored.service_tier = service_tier;
            stored.updated_at = updated_at;
            self.events.emit(DaemonEvent::SessionOptionsChanged {
                session: Box::new(stored.clone()),
            });
        } else {
            stored =
                self.replace_session_for_options(&stored, model, reasoning_effort, service_tier)?;
        }
        Ok(Response::SessionOptionsApplied {
            session: stored,
            outcome,
        })
    }

    /// Replace a provider thread that cannot absorb an option change.
    ///
    /// The normalized transcript is copied and summarized into a one-shot
    /// handoff, so the first turn of the replacement starts a fresh vendor
    /// thread without losing what the conversation was about.
    fn replace_session_for_options(
        &mut self,
        original: &Session,
        model: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    ) -> Result<Session, RpcError> {
        let created_at = now().max(original.created_at.saturating_add(1));
        let replacement = Session {
            id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
            workspace: original.workspace.clone(),
            agent: original.agent.clone(),
            account: original.account.clone(),
            model,
            reasoning_effort,
            service_tier,
            state: SessionState::Idle,
            title: original.title.clone(),
            summary: None,
            vendor_session_id: None,
            access_mode: original.access_mode,
            origin: original.origin.clone(),
            created_at,
            updated_at: created_at,
        };
        {
            let mut conn = self.conn();
            let tx = conn.transaction().map_err(failed)?;
            if original.origin.is_some() {
                session::close_origin(&tx, &original.id).map_err(failed)?;
            }
            session::insert(&tx, &replacement).map_err(failed)?;
            session::copy_transcript(&tx, &original.id, &replacement.id, None).map_err(failed)?;
            let carried = session::transcript(&tx, &replacement.id, None, None).map_err(failed)?;
            let digest =
                crate::handoff::digest(&carried, &original.agent, crate::handoff::DEFAULT_BUDGET);
            if !digest.is_empty() {
                session::set_handoff(&tx, &replacement.id, &digest).map_err(failed)?;
            }
            tx.commit().map_err(failed)?;
        }
        if self.sessions.is_running(&original.id) {
            self.sessions.cancel(&original.id);
        }
        self.events.emit(DaemonEvent::SessionStarted {
            session: Box::new(replacement.clone()),
        });
        Ok(replacement)
    }

    /// Copy a conversation up to a point, to carry on from there.
    ///
    /// On the same agent and login the copy inherits the vendor's thread and
    /// continuing it continues the same conversation. Onto another agent or
    /// another login it cannot — the thread lives in the other vendor's store,
    /// in the other account's directory — so the copy is handed a digest of
    /// the record instead, on its first turn (`crate::handoff`). The record
    /// is the daemon's own, in one shape whatever wrote it, which is what
    /// makes the move possible at all.
    fn fork_session(
        &mut self,
        id: &SessionId,
        after: Option<u64>,
        agent: Option<&str>,
        model: Option<String>,
        account: Option<ginka_protocol::AccountId>,
    ) -> Result<Response, RpcError> {
        let original = self.session(id)?;
        let driver = self.driver(agent.unwrap_or(&original.agent))?;
        let same_agent = driver.id() == original.agent;
        let account = match account {
            Some(account) => crate::account::resolve(&self.settings, driver.id(), Some(&account))
                .map_err(account_error)?,
            // No login named: the original's where the agent is the same,
            // the provider's default where it is not — the original's login
            // belongs to another provider.
            None if same_agent => original.account.clone(),
            None => {
                crate::account::resolve(&self.settings, driver.id(), None).map_err(account_error)?
            }
        };
        let moved = !same_agent || account != original.account;
        let keeps_model_options = same_agent
            && model
                .as_ref()
                .is_none_or(|model| original.model.as_ref() == Some(model));

        let now = now();
        let fork = Session {
            id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
            workspace: original.workspace.clone(),
            agent: driver.id().to_string(),
            account,
            model: match model {
                Some(model) => Some(model),
                // A model id is the vendor's; it does not carry across.
                None if same_agent => original.model.clone(),
                None => None,
            },
            reasoning_effort: keeps_model_options
                .then_some(original.reasoning_effort.clone())
                .flatten(),
            service_tier: keeps_model_options
                .then_some(original.service_tier.clone())
                .flatten(),
            // What the agent may touch is a property of the conversation,
            // and the fork is the same conversation. Where it came from is
            // not: the thread that started the original keeps talking to
            // the original.
            access_mode: original.access_mode,
            origin: None,
            // A fork has not run anything yet.
            state: SessionState::Idle,
            title: Some(match (&original.title, moved) {
                (Some(title), false) => format!("{title} (fork)"),
                (Some(title), true) => format!("{title} ({} fork)", driver.id()),
                (None, false) => "fork".to_string(),
                (None, true) => format!("{} fork", driver.id()),
            }),
            summary: moved.then(|| format!("moved from {}", original.agent)),
            // Inherited only where continuing it would continue the same
            // conversation; elsewhere the digest stands in for it.
            vendor_session_id: (!moved)
                .then_some(original.vendor_session_id.clone())
                .flatten(),
            created_at: now,
            updated_at: now,
        };
        {
            let conn = self.conn();
            session::insert(&conn, &fork).map_err(failed)?;
            session::copy_transcript(&conn, id, &fork.id, after).map_err(failed)?;
            if moved {
                let carried = session::transcript(&conn, &fork.id, None, None).map_err(failed)?;
                let digest = crate::handoff::digest(
                    &carried,
                    &original.agent,
                    crate::handoff::DEFAULT_BUDGET,
                );
                if !digest.is_empty() {
                    session::set_handoff(&conn, &fork.id, &digest).map_err(failed)?;
                }
            }
        }
        self.events.emit(DaemonEvent::SessionStarted {
            session: Box::new(fork.clone()),
        });
        Ok(Response::Session { session: fork })
    }

    /// Send a follow-up to a session, queued if its agent is still working.
    fn send_message(&mut self, id: &SessionId, text: String) -> Result<Response, RpcError> {
        let stored = self.session(id)?;
        let worktree = self.worktree(&stored.workspace)?;
        let driver = self.driver(&stored.agent)?;
        // What a conversation moved from another agent still owes it: the
        // digest rides in front of the first prompt, once, and only while
        // the agent has no thread of its own to have read it in.
        let preamble = match stored.vendor_session_id {
            None => session::handoff(&self.conn(), id).map_err(failed)?,
            Some(_) => None,
        };
        let mut spec = SessionSpec::new(&worktree.path, self.expand_attachments(&text))
            .with_model(stored.model.clone())
            .with_reasoning_effort(stored.reasoning_effort.clone())
            .with_service_tier(stored.service_tier.clone())
            // The mode the conversation was started in: a resume that
            // widened it would be the change N2 reserves for a new session.
            .with_access_mode(stored.access_mode)
            .with_preamble(preamble)
            .with_mcp_servers(self.mcp_servers(&worktree.path));
        // The same login the conversation started on: the vendor's thread
        // lives in its directory (`docs/accounts.md` §5).
        for (key, value) in crate::account::env_layer(
            &self.settings,
            &self.paths,
            &stored.account,
            driver.home_variable(),
        ) {
            spec = spec.with_env(key, value);
        }
        self.sessions
            .send(id.clone(), driver, spec, stored.vendor_session_id)
            .map_err(failed)?;
        Ok(Response::Ack)
    }

    /// Turn every attachment reference in a message into a path the agent can
    /// open. Text with no references comes back unchanged.
    fn expand_attachments(&self, text: &str) -> String {
        crate::attachment::expand_references(
            text,
            &crate::attachment::AttachmentStore::new(self.paths.attachments()),
        )
    }

    /// Put a workspace back to the state a checkpoint captured.
    ///
    /// Restoring is destructive — files written since are removed — so the
    /// state being replaced is snapshotted first. A rewind must never be the
    /// thing that loses work, including work the user wanted after all.
    fn restore(&mut self, id: &CheckpointId) -> Result<Response, RpcError> {
        let checkpoint = checkpoint::get(&self.conn(), id)
            .map_err(failed)?
            .ok_or_else(|| RpcError::not_found(format!("no checkpoint with id {}", id.0)))?;
        let worktree = self.worktree(&checkpoint.workspace)?;

        checkpoint::take(
            &self.conn(),
            &worktree.path,
            checkpoint::TurnRef {
                workspace: &checkpoint.workspace,
                session: &checkpoint.session,
                turn: checkpoint.turn,
            },
            &format!("before restoring: {}", checkpoint.label),
            None,
            now(),
        )
        .map_err(failed)?;

        git::restore_snapshot(&worktree.path, &checkpoint.commit).map_err(failed)?;
        self.events.emit(DaemonEvent::WorkspacesChanged {
            project: worktree.project.clone(),
        });
        Ok(Response::Ack)
    }

    /// Write a commit message for a workspace on a thread of its own, and
    /// push it when it lands (§3.3 N9).
    ///
    /// Not answered inline: the service serialises requests, and a model
    /// that takes thirty seconds would hold every window's next tick for
    /// as long. The driver is the one named, else the workspace's latest
    /// session's — the agent that did the work is the natural one to
    /// describe it — else `claude`; the login follows the same session
    /// where it matches, because that is the one known to be signed in.
    fn generate_commit_message(
        &mut self,
        workspace: WorkspaceId,
        agent: Option<&str>,
        staged: bool,
    ) -> Result<Response, RpcError> {
        let worktree = self.worktree(&workspace)?;
        let latest = session::latest_for_workspace(&self.conn(), &workspace).map_err(failed)?;
        let agent = agent
            .map(str::to_string)
            .or_else(|| latest.as_ref().map(|session| session.agent.clone()))
            .unwrap_or_else(|| "claude".to_string());
        let driver = self.driver(&agent)?;
        let account = match latest.filter(|session| session.agent == driver.id()) {
            Some(session) => session.account,
            None => {
                crate::account::resolve(&self.settings, driver.id(), None).map_err(account_error)?
            }
        };
        let env = crate::account::env_layer(
            &self.settings,
            &self.paths,
            &account,
            driver.home_variable(),
        );
        let events = self.events.clone();
        std::thread::spawn(move || {
            let outcome =
                crate::commit::describe(&worktree.path, staged).and_then(|(files, diff)| {
                    crate::commit::generate(driver.as_ref(), &worktree.path, &env, &files, &diff)
                });
            let (message, error) = match outcome {
                Ok(message) => (Some(message.to_git_message()), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            events.emit(DaemonEvent::CommitMessageGenerated {
                workspace,
                message,
                error,
            });
        });
        Ok(Response::Ack)
    }

    /// The skill library: the user's roots and every project's, or one
    /// project's when named (`docs/roadmap.md` §3.3 N11).
    fn skills(
        &self,
        project: Option<&ProjectName>,
    ) -> Result<crate::skills::SkillCatalog, RpcError> {
        let projects: Vec<(String, std::path::PathBuf)> = match project {
            Some(name) => {
                let found = self
                    .projects()?
                    .into_iter()
                    .find(|project| &project.name == name)
                    .ok_or_else(|| RpcError::not_found(format!("no project named {name}")))?;
                vec![(found.name.0.clone(), found.path.clone())]
            }
            None => self
                .projects()?
                .into_iter()
                .map(|project| (project.name.0.clone(), project.path.clone()))
                .collect(),
        };
        // The user's skills live in their home, which is theirs rather than
        // Ginka's `GINKA_HOME`.
        let roots = crate::skills::default_roots(dirs::home_dir().as_deref(), &projects);
        crate::skills::discover(&roots).map_err(failed)
    }

    /// Resolve a session, or say it is not there.
    fn session(&self, id: &SessionId) -> Result<Session, RpcError> {
        session::get(&self.conn(), id)
            .map_err(failed)?
            .ok_or_else(|| RpcError::not_found(format!("no session with id {id}")))
    }

    /// Resolve a driver by id, naming the ones this build has.
    fn driver(&self, agent: &str) -> Result<Arc<dyn AgentDriver>, RpcError> {
        self.drivers.get(agent).ok_or_else(|| {
            RpcError::not_found(format!(
                "no agent named {agent}; available: {}",
                self.drivers.ids().join(", ")
            ))
        })
    }

    /// Every registered project.
    fn projects(&self) -> Result<Vec<Project>, RpcError> {
        project::list_projects(&self.conn()).map_err(failed)
    }

    /// Resolve a project by name, or say what is registered.
    fn project(&self, name: &ProjectName) -> Result<Project, RpcError> {
        self.projects()?
            .into_iter()
            .find(|project| &project.name == name)
            .ok_or_else(|| self.no_such_project(name))
    }

    fn no_such_project(&self, name: &ProjectName) -> RpcError {
        let known: Vec<String> = self
            .projects()
            .unwrap_or_default()
            .into_iter()
            .map(|project| project.name.0)
            .collect();
        if known.is_empty() {
            RpcError::not_found(format!("no project named {name}; none are registered"))
        } else {
            RpcError::not_found(format!(
                "no project named {name}; registered: {}",
                known.join(", ")
            ))
        }
    }

    /// Resolve a workspace id to the worktree it names.
    fn worktree(&self, workspace: &WorkspaceId) -> Result<Worktree, RpcError> {
        project::find_worktree(&self.conn(), workspace)
            .map_err(failed)?
            .ok_or_else(|| RpcError::not_found(format!("no workspace named {workspace}")))
    }

    /// Everything the dashboard draws for one workspace.
    ///
    /// Status is read from git on demand rather than from a cache: a stale
    /// dirty flag is worse than a subprocess, and the poller that will keep
    /// this warm lands with the status tick in M1. A worktree whose directory
    /// has been deleted under us reports a default status rather than failing
    /// the whole listing.
    fn summarize(&self, worktree: Worktree) -> WorkspaceSummary {
        let status = git::branch_status(&worktree.path).unwrap_or_default();
        let last_commit_at = git::last_commit_time(&worktree.path);
        let session = session::latest_for_workspace(&self.conn(), &worktree.workspace_id())
            .unwrap_or_default();
        WorkspaceSummary {
            worktree,
            status,
            session,
            last_commit_at,
        }
    }
}

/// A conversation's title, from the prompt that opened it.
///
/// The first line, because a prompt's first line is its subject and the rest
/// is detail; bounded, because a title is read in a list.
fn title_from(prompt: &str) -> String {
    let first = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if first.chars().count() <= 72 {
        return first.to_string();
    }
    let kept: String = first.chars().take(71).collect();
    format!("{kept}…")
}

/// Unix seconds.
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Today's date, for the scratch directory a user reads.
///
/// Local rather than UTC: the directory is a human's filing, and work done at
/// 23:00 belongs under the day they did it on.
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl Service {
    /// Ask the same question in one worktree per attempt.
    ///
    /// Each arm goes through the same requests a person would send, so an arm
    /// gets the project's setup and its own checkpoint exactly as a hand-made
    /// workspace does. An arm that fails is reported and the rest carry on:
    /// two answers are worth having even when the third never started.
    fn fan_out(
        &mut self,
        project: ProjectName,
        prefix: &str,
        base: Option<String>,
        prompt: &str,
        attempts: &[ginka_protocol::rpc::Attempt],
    ) -> Result<Response, RpcError> {
        if attempts.is_empty() {
            return Err(RpcError::failed("a fan-out needs at least one attempt"));
        }
        let mut started = Vec::new();
        let mut failed = Vec::new();
        for (index, attempt) in attempts.iter().enumerate() {
            let branch = format!("{prefix}-{}", index + 1);
            let workspace = match self.handle(Request::CreateWorkspace {
                project: project.clone(),
                branch: branch.clone(),
                base: base.clone(),
            }) {
                Ok(Response::Workspace { workspace }) => workspace,
                Ok(other) => {
                    failed.push(format!("{branch}: unexpected answer {other:?}"));
                    continue;
                }
                Err(error) => {
                    failed.push(format!("{branch}: {}", error.message));
                    continue;
                }
            };
            match self.handle(Request::StartSession {
                workspace: workspace.worktree.workspace_id(),
                agent: attempt.agent.clone(),
                prompt: prompt.to_string(),
                model: attempt.model.clone(),
                reasoning_effort: None,
                service_tier: None,
                account: attempt.account.clone(),
                access_mode: None,
                origin: None,
            }) {
                Ok(Response::Session { session }) => started.push(session),
                Ok(other) => failed.push(format!("{branch}: unexpected answer {other:?}")),
                Err(error) => failed.push(format!("{branch}: {}", error.message)),
            }
        }
        Ok(Response::FannedOut { started, failed })
    }

    /// Reconcile every project's worktrees against git.
    ///
    /// The daemon's own tick rather than each client's: a worktree added with
    /// the user's own git, or removed by hand, is something every window
    /// should learn about, and having each of them poll for it is the same
    /// work done once per window.
    pub fn sync(&mut self) {
        let Ok(projects) = self.projects() else {
            return;
        };
        for project in projects {
            let report = {
                let conn = self.conn();
                registry::sync_worktrees(&conn, &project)
            };
            match report {
                Ok(report) if !report.is_empty() => {
                    tracing::debug!(project = project.name.0, "worktrees reconciled");
                    self.events.emit(DaemonEvent::WorkspacesChanged {
                        project: project.name.clone(),
                    });
                }
                Err(error) => {
                    tracing::debug!(%error, project = project.name.0, "could not reconcile")
                }
                _ => {}
            }
        }
    }

    /// Re-read every workspace's git status, and push what changed.
    ///
    /// Only the difference: a status that is the same as last tick is not news,
    /// and a push per workspace per minute would be a push that clients learn
    /// to ignore.
    pub fn poll_statuses(&mut self) {
        let Ok(projects) = self.projects() else {
            return;
        };
        let mut seen = std::collections::HashSet::new();
        for project in projects {
            let Ok(worktrees) = project::list_worktrees(&self.conn(), &project.name) else {
                continue;
            };
            for worktree in worktrees {
                let id = worktree.workspace_id();
                seen.insert(id.clone());
                let Ok(status) = git::branch_status(&worktree.path) else {
                    continue;
                };
                if self.statuses.get(&id) == Some(&status) {
                    continue;
                }
                self.statuses.insert(id.clone(), status);
                self.events.emit(DaemonEvent::WorkspaceStatusChanged {
                    workspace: id,
                    status,
                });
            }
        }
        // A workspace that is gone is not worth remembering the status of.
        self.statuses.retain(|id, _| seen.contains(id));
    }

    /// Do what the project asked for in a new worktree.
    ///
    /// A fresh checkout has none of the files the repository deliberately does
    /// not track, and an agent started there fails on its first command for a
    /// reason that has nothing to do with its task. What went wrong is logged
    /// rather than returned: the worktree exists by now, and removing it
    /// because an install failed would throw away the branch the user asked
    /// for.
    fn set_up(&self, project: &std::path::Path, worktree: &std::path::Path) {
        let setup = match crate::setup::read(project) {
            Ok(setup) => setup,
            Err(error) => {
                tracing::warn!(%error, "a project's setup file could not be read");
                return;
            }
        };
        if setup.copy.is_empty() && setup.commands.is_empty() {
            return;
        }
        let report = crate::setup::run(project, worktree, &setup);
        for problem in &report.problems {
            tracing::warn!(problem, worktree = %worktree.display(), "setting up the worktree");
        }
        tracing::info!(
            copied = report.copied.len(),
            ran = report.ran.len(),
            "ran the project's setup"
        );
    }
}

/// Turn a domain failure into the protocol's generic failure.
/// What an account runs on: its provider's driver, and the environment that
/// points that driver's CLI at the account's directory.
type AccountRuntime = (Arc<dyn AgentDriver>, Vec<(String, String)>);

/// An account error as a client sees it: a name that is not there is a
/// not-found, and the rest are refusals with the reason in them.
fn account_error(error: crate::account::AccountError) -> RpcError {
    match error {
        crate::account::AccountError::Unknown(_) => RpcError::not_found(error.to_string()),
        other => RpcError::failed(other.to_string()),
    }
}

fn failed(error: impl std::fmt::Display) -> RpcError {
    RpcError::failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_with_no_clients_still_answers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("state"));
        let mut service = Service::open(paths, Arc::new(NullSink)).unwrap();
        assert_eq!(service.handle(Request::Ping).unwrap(), Response::Ack);
    }

    #[test]
    fn a_conversation_is_titled_by_what_was_asked_of_it() {
        assert_eq!(
            title_from("Fix the parser\n\nIt drops the last token."),
            "Fix the parser",
            "the first line is the subject; the rest is detail"
        );
        assert_eq!(title_from("   \n  spaced  \n"), "spaced");
        assert_eq!(title_from(""), "");
        let long = title_from(&"x".repeat(200));
        assert_eq!(long.chars().count(), 72, "a title is read in a list");
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_request_naming_a_workspace_that_is_not_there_is_not_found() {
        // Every method in the protocol is implemented, so the interesting
        // failure is a bad argument rather than a missing capability.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("state"));
        let mut service = Service::open(paths, Arc::new(NullSink)).unwrap();
        let error = service
            .handle(Request::ListCheckpoints {
                workspace: WorkspaceId("comet/harbor".into()),
            })
            .unwrap_err();
        assert_eq!(error.code, "not_found");
    }

    #[test]
    fn asking_about_a_session_that_does_not_exist_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("state"));
        let mut service = Service::open(paths, Arc::new(NullSink)).unwrap();
        let error = service
            .handle(Request::CancelSession {
                session: SessionId("absent".into()),
            })
            .unwrap_err();
        assert_eq!(error.code, "not_found");
    }
}
