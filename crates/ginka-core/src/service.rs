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
use crate::driver::{AgentDriver, Registry, SessionSpec};
use crate::registry;
use crate::{Paths, git, project, session};
use anyhow::Result;
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::ids::slugify;
use ginka_protocol::model::{
    AgentStatus, ChangeSource, Changes, Project, ProjectKind, Session, SessionState,
    WorkspaceSummary, Worktree,
};
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
        Self {
            sessions: Supervisor::new(conn.clone(), events.clone(), settings.checkpoint_limit),
            paths,
            terminals: crate::terminal::Terminals::new(events.clone()),
            conn,
            events,
            drivers: Arc::new(Registry::with_defaults()),
            statuses: std::collections::HashMap::new(),
            agents: None,
        }
    }

    /// Use these drivers rather than the ones this build ships.
    ///
    /// This is how the tests put a scripted agent behind the `claude` id, and
    /// how a user's configured binaries will be installed later.
    pub fn with_drivers(mut self, drivers: Registry) -> Self {
        self.drivers = Arc::new(drivers);
        self
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
            Request::AddProject { path } => {
                let project = registry::register_project(&self.conn(), &path).map_err(failed)?;
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

            Request::ListAgents => Ok(Response::Agents {
                agents: self.agents(),
            }),
            Request::ListSessions { workspace } => Ok(Response::Sessions {
                sessions: session::list(&self.conn(), workspace.as_ref()).map_err(failed)?,
            }),
            Request::StartSession {
                workspace,
                agent,
                prompt,
                model,
            } => self.start_session(workspace, &agent, prompt, model),
            Request::FanOut {
                project,
                branch_prefix,
                base,
                prompt,
                attempts,
            } => self.fan_out(project, &branch_prefix, base, &prompt, &attempts),
            Request::SendMessage { session, text } => self.send_message(&session, text),
            Request::RespondToAgent {
                session, response, ..
            } => {
                // Every driver this build ships runs its vendor's
                // non-interactive mode, which never asks a question mid-turn.
                // An answer is therefore a follow-up like any other; drivers
                // that can interrupt a turn will override this.
                self.send_message(&session, response)
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
            Request::ForkSession { session, after } => {
                let original = self.session(&session)?;
                let now = now();
                let fork = Session {
                    id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
                    workspace: original.workspace.clone(),
                    agent: original.agent.clone(),
                    model: original.model.clone(),
                    // A fork has not run anything yet, and inherits the
                    // vendor's own conversation so that continuing it
                    // continues where the original was.
                    state: SessionState::Idle,
                    title: Some(match &original.title {
                        Some(title) => format!("{title} (fork)"),
                        None => "fork".to_string(),
                    }),
                    summary: None,
                    vendor_session_id: original.vendor_session_id.clone(),
                    created_at: now,
                    updated_at: now,
                };
                {
                    let conn = self.conn();
                    session::insert(&conn, &fork).map_err(failed)?;
                    session::copy_transcript(&conn, &session, &fork.id, after).map_err(failed)?;
                }
                self.events.emit(DaemonEvent::SessionStarted {
                    session: fork.clone(),
                });
                Ok(Response::Session { session: fork })
            }
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

            Request::Shutdown => {
                // The transport is listening for this: it is the one event a
                // client cannot poll for after the fact.
                self.events.emit(DaemonEvent::Shutdown);
                Ok(Response::Ack)
            }
        }
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

    /// Start an agent in a workspace.
    fn start_session(
        &mut self,
        workspace: WorkspaceId,
        agent: &str,
        prompt: String,
        model: Option<String>,
    ) -> Result<Response, RpcError> {
        let worktree = self.worktree(&workspace)?;
        let driver = self.driver(agent)?;
        let now = now();
        let session = Session {
            id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
            workspace,
            agent: driver.id().to_string(),
            model: model.clone(),
            state: SessionState::Starting,
            // What this conversation is about, taken from what was asked. The
            // user can rename it; nothing else writes it, because the agent's
            // own summary is a different thing and changes every turn.
            title: Some(title_from(&prompt)),
            summary: None,
            vendor_session_id: None,
            created_at: now,
            updated_at: now,
        };
        session::insert(&self.conn(), &session).map_err(failed)?;
        self.events.emit(DaemonEvent::SessionStarted {
            session: session.clone(),
        });

        // The agent reads files, not URIs: an attachment the user mentioned
        // reaches it as a path on this host (§3.3 N6). The transcript keeps
        // the reference, so the window can still draw the attachment.
        let spec =
            SessionSpec::new(worktree.path, self.expand_attachments(&prompt)).with_model(model);
        self.sessions
            .start(session.id.clone(), driver, spec)
            .map_err(failed)?;
        Ok(Response::Session { session })
    }

    /// Send a follow-up to a session, queued if its agent is still working.
    fn send_message(&mut self, id: &SessionId, text: String) -> Result<Response, RpcError> {
        let stored = self.session(id)?;
        let worktree = self.worktree(&stored.workspace)?;
        let driver = self.driver(&stored.agent)?;
        let spec = SessionSpec::new(worktree.path, self.expand_attachments(&text))
            .with_model(stored.model.clone());
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
