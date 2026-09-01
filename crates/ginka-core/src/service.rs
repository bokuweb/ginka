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

use crate::registry;
use crate::{Paths, git, project};
use anyhow::Result;
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::ids::slugify;
use ginka_protocol::model::{Project, ProjectKind, WorkspaceSummary, Worktree};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{ProjectName, RpcError, WorkspaceId};
use rusqlite::Connection;
use std::sync::Arc;

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
    conn: Connection,
    events: Arc<dyn EventSink>,
}

impl Service {
    /// Build a service around an already-open database.
    pub fn new(paths: Paths, conn: Connection, events: Arc<dyn EventSink>) -> Self {
        Self {
            paths,
            conn,
            events,
        }
    }

    /// Open the database `paths` points at, running migrations, and build a
    /// service around it.
    pub fn open(paths: Paths, events: Arc<dyn EventSink>) -> Result<Self> {
        paths.ensure()?;
        let conn = crate::db::open(&paths.database())?;
        Ok(Self::new(paths, conn, events))
    }

    /// Where this service keeps its state.
    pub fn paths(&self) -> &Paths {
        &self.paths
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
                let project = registry::register_project(&self.conn, &path).map_err(failed)?;
                registry::sync_worktrees(&self.conn, &project).map_err(failed)?;
                self.events.emit(DaemonEvent::ProjectsChanged);
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name.clone(),
                });
                Ok(Response::Project { project })
            }
            Request::RemoveProject { project } => {
                if !project::remove_project(&self.conn, &project).map_err(failed)? {
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
                    registry::sync_worktrees(&self.conn, project).map_err(failed)?;
                    for worktree in
                        project::list_worktrees(&self.conn, &project.name).map_err(failed)?
                    {
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
                registry::sync_worktrees(&self.conn, &project).map_err(failed)?;
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name.clone(),
                });

                let worktree = project::list_worktrees(&self.conn, &project.name)
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
            Request::RemoveWorkspace { workspace, force } => {
                let worktree = self.worktree(&workspace)?;
                let project = self.project(&worktree.project)?;
                git::remove_worktree(&project.path, &worktree.path, force).map_err(failed)?;
                registry::sync_worktrees(&self.conn, &project).map_err(failed)?;
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: project.name,
                });
                Ok(Response::Ack)
            }
            Request::PinWorkspace { workspace, pinned } => {
                if !project::set_pinned(&self.conn, &workspace, pinned).map_err(failed)? {
                    return Err(RpcError::not_found(format!(
                        "no workspace named {workspace}"
                    )));
                }
                if let Some((project, _)) = workspace.parts() {
                    self.events.emit(DaemonEvent::WorkspacesChanged { project });
                }
                Ok(Response::Ack)
            }

            Request::Shutdown => {
                // The transport is listening for this: it is the one event a
                // client cannot poll for after the fact.
                self.events.emit(DaemonEvent::Shutdown);
                Ok(Response::Ack)
            }

            other => Err(RpcError {
                code: "unsupported".into(),
                message: format!("this daemon does not implement {other:?} yet"),
            }),
        }
    }

    /// Every registered project.
    fn projects(&self) -> Result<Vec<Project>, RpcError> {
        project::list_projects(&self.conn).map_err(failed)
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
        project::find_worktree(&self.conn, workspace)
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
        WorkspaceSummary {
            worktree,
            status,
            session: None,
            last_commit_at,
        }
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
    fn an_unimplemented_request_says_so_rather_than_answering_ack() {
        // A client must be able to tell "done" from "this build cannot do it".
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("state"));
        let mut service = Service::open(paths, Arc::new(NullSink)).unwrap();
        let error = service
            .handle(Request::ListCheckpoints {
                workspace: WorkspaceId("comet/harbor".into()),
            })
            .unwrap_err();
        assert_eq!(error.code, "unsupported");
    }
}
