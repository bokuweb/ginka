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
//! What waits on the network is the exception: [`handle_shared`] runs that
//! wait with the service unlocked, between two locked steps.

use crate::agent::Supervisor;
use crate::checkpoint;
use crate::connector::{self, ConnectorControl};
use crate::driver::{AgentDriver, Registry, SessionSpec};
use crate::registry;
use crate::{Paths, git, project, session};
use anyhow::{Context as _, Result};
use ginka_protocol::event::{AgentEvent, DaemonEvent};
use ginka_protocol::ids::slugify;
use ginka_protocol::model::TranscriptPayload;
use ginka_protocol::model::{
    AgentStatus, ChangeSource, Changes, Project, ProjectKind, Session, SessionOrigin, SessionState,
    WorkspaceSummary, Worktree,
};
use ginka_protocol::provider::{AccessMode, OptionOutcome, ProviderKind, SessionOptions};
use ginka_protocol::rpc::{ProviderSetting, Request, Response};
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

/// What becomes of a request's network outcome, run with the service
/// locked again.
type Finish = Box<dyn FnOnce(&mut Service) -> Result<Response, RpcError> + Send>;

/// How far [`Service::begin`] got with a request.
// Large only because `Response` is: a step lives for one request, never in
// a collection, so boxing the answer would buy nothing.
#[allow(clippy::large_enum_variant)]
enum Step {
    /// Answered, under the lock.
    Done(Result<Response, RpcError>),
    /// Waiting on the network is left: run it unlocked, then its [`Finish`]
    /// locked.
    Away(Box<dyn FnOnce() -> Finish + Send>),
}

/// Search a project's worktrees for paths and lines matching `query`, up
/// to `limit` of each across all of them, in the order given.
fn search_worktrees(
    worktrees: &[Worktree],
    query: &str,
    limit: Option<u32>,
) -> Result<(
    Vec<ginka_protocol::model::WorkspaceFileMatch>,
    Vec<ginka_protocol::model::WorkspaceContentMatch>,
)> {
    let limit = limit
        .map(|limit| limit as usize)
        .unwrap_or(crate::files::DEFAULT_LIMIT);
    let mut remaining_files = limit;
    let mut remaining_matches = limit;
    let mut files = Vec::new();
    let mut matches = Vec::new();
    for worktree in worktrees {
        if remaining_files == 0 && remaining_matches == 0 {
            break;
        }
        let workspace = worktree.workspace_id();
        if remaining_files > 0 {
            let paths = crate::files::list(&worktree.path)?;
            let found = crate::files::search(&paths, query, remaining_files);
            remaining_files = remaining_files.saturating_sub(found.len());
            files.extend(
                found
                    .into_iter()
                    .map(|file| ginka_protocol::model::WorkspaceFileMatch {
                        workspace: workspace.clone(),
                        path: file.path,
                    }),
            );
        }
        let found = crate::files::search_content(&worktree.path, query, remaining_matches)?;
        remaining_matches = remaining_matches.saturating_sub(found.len());
        matches.extend(
            found
                .into_iter()
                .map(|hit| ginka_protocol::model::WorkspaceContentMatch {
                    workspace: workspace.clone(),
                    path: hit.path,
                    line: hit.line,
                    text: hit.text,
                }),
        );
    }
    Ok((files, matches))
}

/// Answer a request for a service shared between connections, holding its
/// lock only while state is read or written.
///
/// A push, a pull, a sync, opening a pull request and reading its checks
/// wait on a remote for as long as it takes, a commit or a merge on the
/// project's hooks, and a search on the size of the repository; the service is unlocked for that wait, so every other window and command is still answered. What the
/// wait found is recorded under the lock again.
pub fn handle_shared(service: &Mutex<Service>, request: Request) -> Result<Response, RpcError> {
    let lock = || {
        service
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    };
    let work = match lock().begin(request) {
        Step::Done(answer) => return answer,
        Step::Away(work) => work,
    };
    let finish = work();
    finish(&mut lock())
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
    /// The pull request each workspace's branch was last seen with.
    ///
    /// Filled by the daemon's poller from `gh`, never by a listing: the
    /// sidebar must not wait on the network to draw.
    pull_requests: std::collections::HashMap<WorkspaceId, ginka_protocol::model::PullRequest>,
    /// What each recent turn added, for line attribution — read once per
    /// turn, since its snapshots never change.
    turn_lines: crate::attribution::TurnLinesCache,
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
    /// The public rate table, as last cached, for pricing the turns a vendor
    /// did not (§3.3 N13). Replaced by the daemon's daily refresh.
    rates: Option<crate::usage::RateTable>,
    /// Whether the drivers are the ones the settings describe, and so are
    /// built again when the settings file changes. Not for a service given
    /// its drivers, which is how a test puts a scripted agent in.
    drivers_follow_settings: bool,
    /// Where vendor session logs are read from, when a test says; otherwise
    /// the vendors' own directories (`usage::scan`).
    vendor_logs: Option<Vec<(std::path::PathBuf, crate::usage::scan::Format)>>,
    /// The home whose skills directories are read and written, when a test
    /// says; otherwise the daemon user's.
    skills_home: Option<std::path::PathBuf>,
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
    /// Where the agents' own CLIs keep their conversations, when a test
    /// says; otherwise the system login's directories (`cli_sessions`).
    cli_roots: Option<crate::cli_sessions::Roots>,
    /// What was last read from the CLIs' files, so reopening the list does
    /// not read them all again.
    cli_index: crate::cli_sessions::Index,
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
        let rates = crate::usage::RateTable::load(&paths.rates_cache())
            .ok()
            .flatten();
        Self {
            cli,
            sessions: Supervisor::new(
                conn.clone(),
                events.clone(),
                settings.checkpoint_limit,
                crate::blob::BlobStore::new(paths.blobs()),
            )
            .with_keep_awake(settings.keep_awake)
            .with_resume_after_limit(settings.resume_after_limit),
            paths,
            terminals: crate::terminal::Terminals::new(events.clone()),
            conn,
            events,
            drivers: Arc::new(Registry::with_defaults()),
            statuses: std::collections::HashMap::new(),
            pull_requests: std::collections::HashMap::new(),
            turn_lines: Default::default(),
            agents: None,
            settings,
            accounts: None,
            rates,
            drivers_follow_settings: false,
            vendor_logs: None,
            skills_home: None,
            accounts_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            connectors: Vec::new(),
            cli_roots: None,
            cli_index: crate::cli_sessions::Index::default(),
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

    /// Use the drivers the settings describe — each agent's binary and
    /// environment — and build them again when the settings file changes.
    pub fn with_settings_drivers(mut self) -> Self {
        self.drivers = Arc::new(Registry::from_settings(&self.settings));
        self.drivers_follow_settings = true;
        self
    }

    /// Read vendor session logs from these places instead of the vendors'
    /// own directories. How a test keeps the scanner out of the real home.
    pub fn with_vendor_logs(
        mut self,
        roots: Vec<(std::path::PathBuf, crate::usage::scan::Format)>,
    ) -> Self {
        self.vendor_logs = Some(roots);
        self
    }

    /// Read the CLIs' conversations from these directories instead of the
    /// system login's. How a test keeps the list out of the real home.
    pub fn with_cli_roots(mut self, roots: crate::cli_sessions::Roots) -> Self {
        self.cli_roots = Some(roots);
        self
    }

    /// Read and write skills under this home instead of the daemon user's.
    pub fn with_skills_home(mut self, home: impl Into<std::path::PathBuf>) -> Self {
        self.skills_home = Some(home.into());
        self
    }

    fn home(&self) -> Option<std::path::PathBuf> {
        self.skills_home.clone().or_else(dirs::home_dir)
    }

    /// Where the vendors keep their session logs: Claude Code under its
    /// config directory's `projects`, Codex under its home's `sessions` — the
    /// default login's and each added login's.
    fn vendor_log_roots(&self) -> Vec<(std::path::PathBuf, crate::usage::scan::Format)> {
        use crate::usage::scan::Format;
        if let Some(roots) = &self.vendor_logs {
            return roots.clone();
        }
        if !self.settings.scan_vendor_logs {
            return Vec::new();
        }
        let mut roots = Vec::new();
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            roots.push((home.join(".claude/projects"), Format::Claude));
            let codex = std::env::var_os("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            roots.push((codex.join("sessions"), Format::Codex));
        }
        for (id, account) in &self.settings.accounts {
            let home =
                crate::account::home_dir(&self.paths, &ginka_protocol::AccountId(id.clone()));
            match account.provider {
                ginka_protocol::ProviderKind::Claude => {
                    roots.push((home.join("projects"), Format::Claude))
                }
                ginka_protocol::ProviderKind::Codex => {
                    roots.push((home.join("sessions"), Format::Codex))
                }
                _ => {}
            }
        }
        roots
    }

    /// What a scan of the vendor logs needs: where they are and where each
    /// was read to. Taken under the service's lock; the reading is not.
    pub fn outside_scan_plan(
        &self,
    ) -> (
        Vec<(std::path::PathBuf, crate::usage::scan::Format)>,
        std::collections::HashMap<String, crate::usage::scan::Watermark>,
    ) {
        let marks = crate::usage::watermarks(&self.conn()).unwrap_or_default();
        (self.vendor_log_roots(), marks)
    }

    /// Keep what a scan read, and where it got to. Returns how many requests
    /// were new.
    pub fn record_outside(
        &mut self,
        records: &[crate::usage::scan::Record],
        moved: &[(String, crate::usage::scan::Watermark)],
    ) -> usize {
        let conn = self.conn();
        let added = match crate::usage::store_outside(&conn, records) {
            Ok(added) => added,
            Err(error) => {
                tracing::error!(%error, "could not keep usage read from vendor logs");
                return 0;
            }
        };
        for (file, mark) in moved {
            if let Err(error) = crate::usage::save_watermark(&conn, file, mark) {
                tracing::warn!(%error, file, "could not remember where a vendor log was read to");
            }
        }
        added
    }

    /// Plan, read and keep a scan in one go — for tests, and for anything
    /// that does not mind holding the service while files are read.
    pub fn scan_outside_now(&mut self) -> usize {
        let (roots, marks) = self.outside_scan_plan();
        let (records, moved) = crate::usage::scan::collect(&roots, &marks);
        self.record_outside(&records, &moved)
    }

    /// Each project's name and its folders — its checkout and its
    /// worktrees — for placing usage by where it ran.
    fn project_folders(&self) -> Vec<(String, std::path::PathBuf)> {
        let Ok(projects) = self.projects() else {
            return Vec::new();
        };
        let mut folders = Vec::new();
        for project in projects {
            folders.push((project.name.0.clone(), project.path.clone()));
            for worktree in project::list_worktrees(&self.conn(), &project.name).unwrap_or_default()
            {
                folders.push((project.name.0.clone(), worktree.path));
            }
        }
        folders
    }

    /// Read the settings file again if it says something other than what the
    /// daemon is running with — someone edited it by hand — and say whether
    /// it did. A file that does not parse is left for the reader to fix, and
    /// the running settings stay. Keep-awake is read at start only.
    pub fn reload_settings_if_changed(&mut self) -> bool {
        let path = self.paths.daemon_settings();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let settings: crate::settings::DaemonSettings = match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "the edited settings do not parse; keeping the running ones");
                return false;
            }
        };
        if settings == self.settings {
            return false;
        }
        tracing::info!(path = %path.display(), "settings changed on disk; reading them again");
        self.take_settings(settings);
        true
    }

    /// Run with these settings from now on.
    fn take_settings(&mut self, settings: crate::settings::DaemonSettings) {
        self.settings = settings;
        // What was probed under the old settings is stale.
        self.accounts = None;
        self.agents = None;
        if self.drivers_follow_settings {
            self.drivers = Arc::new(Registry::from_settings(&self.settings));
        }
    }

    /// Hand agents this `ginka` command as their MCP bridge, rather than the
    /// one found beside the daemon. How a test says where the CLI is.
    pub fn with_cli(mut self, cli: impl Into<std::path::PathBuf>) -> Self {
        self.cli = Some(cli.into());
        self
    }

    /// The MCP servers an agent starting in `worktree` is told about.
    fn mcp_servers(
        &self,
        worktree: &std::path::Path,
        session: &SessionId,
    ) -> Vec<crate::tools::McpServer> {
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
            Some(&session.0),
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
        match self.begin(request) {
            Step::Done(answer) => answer,
            Step::Away(work) => work()(self),
        }
    }

    /// Start a request: answer it, or — when it has to wait on the network —
    /// hand back the waiting to be done with the service unlocked
    /// ([`handle_shared`]) and what to do with its outcome afterwards.
    fn begin(&mut self, request: Request) -> Step {
        match request {
            Request::Push {
                workspace,
                force_with_lease,
            } => self.away_in(
                &workspace,
                move |path| {
                    if force_with_lease {
                        git::push_with_lease(path)
                    } else {
                        git::push(path)
                    }
                },
                |service, worktree, _| {
                    service.events.emit(DaemonEvent::WorkspacesChanged {
                        project: worktree.project.clone(),
                    });
                    Ok(Response::Ack)
                },
            ),
            // The project's hooks run in a commit, for as long as they like —
            // and one may itself ask the daemon something.
            Request::Commit {
                workspace,
                message,
                all,
                amend,
            } => self.away_in(
                &workspace,
                move |path| {
                    if amend {
                        git::amend(path, &message, all)
                    } else {
                        anyhow::ensure!(!message.trim().is_empty(), "a commit needs a message");
                        git::commit(path, &message, all)
                    }
                },
                |service, worktree, commit| {
                    // The branch moved, so what the sidebar says about it is stale.
                    service.events.emit(DaemonEvent::WorkspacesChanged {
                        project: worktree.project.clone(),
                    });
                    Ok(Response::Committed { commit })
                },
            ),
            Request::MergeWorkspace {
                workspace,
                into,
                message,
            } => {
                let planned = self.worktree(&workspace).and_then(|worktree| {
                    let project = self.project(&worktree.project)?;
                    Ok((worktree, project))
                });
                let (worktree, project) = match planned {
                    Ok(planned) => planned,
                    Err(error) => return Step::Done(Err(error)),
                };
                let into = into
                    .filter(|branch| !branch.trim().is_empty())
                    .or_else(|| git::current_branch(&project.path))
                    .unwrap_or_else(|| project.default_branch.clone());
                if into == worktree.branch {
                    return Step::Done(Err(RpcError::failed(format!(
                        "{into} is this workspace's own branch; choose another to merge into"
                    ))));
                }
                // Both the commit of what is left and the merge run the
                // project's hooks.
                Step::Away(Box::new(move || {
                    let merged = (|| {
                        if git::branch_status(&worktree.path)?.dirty {
                            let message = message
                                .filter(|message| !message.trim().is_empty())
                                .context(
                                    "the workspace has uncommitted work; give a message to commit it with",
                                )?;
                            git::commit(&worktree.path, &message, true)?;
                        }
                        git::merge_into(&project.path, &worktree.branch, &into)
                    })();
                    Box::new(move |service: &mut Service| {
                        let outcome = merged.map_err(failed)?;
                        service.events.emit(DaemonEvent::WorkspacesChanged {
                            project: worktree.project.clone(),
                        });
                        Ok(Response::Merged { outcome })
                    })
                }))
            }
            // Searching walks and reads the whole worktree, which in a large
            // repository is long enough to stall every other client — and the
            // reader is typing, so it is asked again on every keystroke.
            Request::WorkspaceFiles {
                workspace,
                query,
                limit,
            } => self.away_in(
                &workspace,
                move |path| {
                    let paths = crate::files::list(path)?;
                    Ok(crate::files::search(
                        &paths,
                        query.as_deref().unwrap_or_default(),
                        limit
                            .map(|limit| limit as usize)
                            .unwrap_or(crate::files::DEFAULT_LIMIT),
                    ))
                },
                |_, _, files| Ok(Response::Files { files }),
            ),
            Request::SearchContent {
                workspace,
                query,
                limit,
            } => self.away_in(
                &workspace,
                move |path| {
                    let limit = limit
                        .map(|limit| limit as usize)
                        .unwrap_or(crate::files::DEFAULT_LIMIT);
                    crate::files::search_content(path, &query, limit)
                },
                |_, _, matches| Ok(Response::Matches { matches }),
            ),
            Request::SearchProject {
                project,
                query,
                limit,
            } => {
                let planned = self
                    .project(&project)
                    .and_then(|_| project::list_worktrees(&self.conn(), &project).map_err(failed));
                let worktrees: Vec<Worktree> = match planned {
                    Ok(worktrees) => worktrees
                        .into_iter()
                        .filter(|worktree| !worktree.archived)
                        .collect(),
                    Err(error) => return Step::Done(Err(error)),
                };
                Step::Away(Box::new(move || {
                    let found = search_worktrees(&worktrees, &query, limit);
                    Box::new(move |_: &mut Service| {
                        let (files, matches) = found.map_err(failed)?;
                        Ok(Response::WorkspaceMatches { files, matches })
                    })
                }))
            }
            Request::Pull { workspace } => self.away_in(
                &workspace,
                git::pull_fast_forward,
                |service, worktree, _| {
                    service.events.emit(DaemonEvent::WorkspacesChanged {
                        project: worktree.project.clone(),
                    });
                    Ok(Response::Ack)
                },
            ),
            Request::Sync { workspace } => {
                self.away_in(&workspace, git::sync, |service, worktree, synced| {
                    service.events.emit(DaemonEvent::WorkspacesChanged {
                        project: worktree.project.clone(),
                    });
                    Ok(Response::Synced {
                        pulled: synced.pulled,
                        pushed: synced.pushed,
                    })
                })
            }
            Request::CreatePullRequest { workspace, draft } => self.away_in(
                &workspace,
                move |path| git::create_pull_request(path, draft),
                move |service, worktree, url| {
                    // Known at once rather than on the poller's next beat: the
                    // reader just asked for it and is looking at the row.
                    if let Some(number) = git::pull_request_number(&url) {
                        let state = if draft {
                            ginka_protocol::model::PullRequestState::Draft
                        } else {
                            ginka_protocol::model::PullRequestState::Open
                        };
                        let pull_request = ginka_protocol::model::PullRequest {
                            number,
                            url: url.clone(),
                            state,
                        };
                        service.note_pull_request(worktree.workspace_id(), Some(pull_request));
                    }
                    // The push moved the branch's upstream, which the sidebar shows.
                    service.events.emit(DaemonEvent::WorkspacesChanged {
                        project: worktree.project.clone(),
                    });
                    Ok(Response::PullRequest { url })
                },
            ),
            Request::PullRequestChecks { workspace } => {
                self.away_in(&workspace, crate::checks::read, |_, _, checks| {
                    Ok(Response::Checks { checks })
                })
            }
            Request::FixFailingChecks { workspace, agent } => self.away_in(
                &workspace,
                crate::checks::read,
                move |service, worktree, checks| {
                    let workspace = worktree.workspace_id();
                    let Some(prompt) = crate::checks::fix_prompt(&checks) else {
                        return Err(RpcError::failed(format!(
                            "no check on {workspace}'s pull request has failed"
                        )));
                    };
                    service.hand_to_agent(workspace, agent, prompt)
                },
            ),
            other => Step::Done(self.answer(other)),
        }
    }

    /// Plan network work on a workspace's worktree: the worktree is resolved
    /// now, under the lock; `work` runs on its path without it; `finish`
    /// takes the lock back to record what came of it. A failed `work` is the
    /// request's error and `finish` does not run.
    fn away_in<T: Send + 'static>(
        &mut self,
        workspace: &WorkspaceId,
        work: impl FnOnce(&std::path::Path) -> Result<T> + Send + 'static,
        finish: impl FnOnce(&mut Service, Worktree, T) -> Result<Response, RpcError> + Send + 'static,
    ) -> Step {
        let worktree = match self.worktree(workspace) {
            Ok(worktree) => worktree,
            Err(error) => return Step::Done(Err(error)),
        };
        Step::Away(Box::new(move || {
            let outcome = work(&worktree.path);
            Box::new(move |service: &mut Service| {
                let outcome = outcome.map_err(failed)?;
                finish(service, worktree, outcome)
            })
        }))
    }

    /// Answer a request that needs nothing from the network, start to end
    /// under the lock.
    fn answer(&mut self, request: Request) -> Result<Response, RpcError> {
        match request {
            Request::Ping => Ok(Response::Ack),
            // Waiting on a remote or the project's hooks: [`Service::begin`]
            // splits these.
            Request::Commit { .. }
            | Request::MergeWorkspace { .. }
            | Request::Push { .. }
            | Request::Pull { .. }
            | Request::Sync { .. }
            | Request::CreatePullRequest { .. }
            | Request::PullRequestChecks { .. }
            | Request::FixFailingChecks { .. }
            | Request::WorkspaceFiles { .. }
            | Request::SearchContent { .. }
            | Request::SearchProject { .. } => self.handle(request),

            Request::ListProjects => Ok(Response::Projects {
                projects: self.projects()?,
            }),
            Request::SetProjectLabel { project, label } => {
                let label = label.trim();
                let label = (!label.is_empty()).then_some(label);
                if !project::set_label(&self.conn(), &project, label).map_err(failed)? {
                    return Err(RpcError::not_found(format!("no project named {project}")));
                }
                self.events.emit(DaemonEvent::ProjectsChanged);
                Ok(Response::Ack)
            }
            Request::MoveProject { project, index } => {
                if !project::move_to(&self.conn(), &project, index as usize).map_err(failed)? {
                    return Err(RpcError::not_found(format!("no project named {project}")));
                }
                self.events.emit(DaemonEvent::ProjectsChanged);
                Ok(Response::Ack)
            }
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
                self.refuse_while_running(|workspace| {
                    workspace.parts().is_some_and(|(owner, _)| owner == project)
                })?;
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
                // The ignored files the repository asks every worktree to
                // start with (`.worktreeinclude`). Best-effort: a missing
                // `.env` should not cost the reader the worktree itself.
                match crate::worktree_include::copy_included(&project.path, &path) {
                    Ok(copied) if !copied.is_empty() => {
                        tracing::info!(count = copied.len(), "copied .worktreeinclude files");
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "could not copy .worktreeinclude files"),
                }
                // …and the heavy ignored directories it shares rather than
                // copies (`.worktreeshare`), on the same best-effort terms.
                match crate::worktree_include::link_shared(&project.path, &path) {
                    Ok(linked) if !linked.is_empty() => {
                        tracing::info!(?linked, "linked .worktreeshare directories");
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "could not link .worktreeshare directories")
                    }
                }
                // git records the resolved path, which on macOS differs from
                // the one we asked for (/var against /private/var).
                let path = path.canonicalize().unwrap_or(path);
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
                self.set_up(&project.path, &worktree.path, &worktree.workspace_id());
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
                self.refuse_while_running(|running| *running == workspace)?;
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

            Request::SetWorkspaceStatus {
                workspace,
                path,
                note,
            } => {
                let workspace = match (workspace, path) {
                    (Some(workspace), _) => workspace,
                    (None, Some(path)) => self.workspace_holding(&path)?,
                    (None, None) => {
                        return Err(RpcError::failed("name a workspace, or the path inside one"));
                    }
                };
                if !project::set_status_note(&self.conn(), &workspace, note.as_deref(), now())
                    .map_err(failed)?
                {
                    return Err(RpcError::not_found(format!(
                        "no workspace named {workspace}"
                    )));
                }
                if let Some((project, _)) = workspace.parts() {
                    self.events.emit(DaemonEvent::WorkspacesChanged { project });
                }
                Ok(Response::Ack)
            }

            Request::CheckAgentUpdates => {
                let agents = self.agents();
                let events = self.events.clone();
                // The registry is the network: never under the service's
                // lock, and a slow answer only delays its own event.
                std::thread::spawn(move || {
                    let updates = crate::agent_updates::check(&agents, |url| {
                        Ok(ureq::get(url)
                            .call()?
                            .body_mut()
                            .with_config()
                            .limit(256 * 1024)
                            .read_to_string()?)
                    });
                    events.emit(DaemonEvent::AgentUpdatesChecked { updates });
                });
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
            Request::SelectAccount { id } => {
                crate::account::select(&mut self.settings, &id).map_err(account_error)?;
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
            Request::QueuedMessages { session } => {
                self.session(&session)?;
                Ok(Response::QueuedMessages {
                    messages: self.sessions.queued_messages(&session),
                    can_send_now: self.sessions.can_send_queued_message_now(&session),
                    paused: self.sessions.queue_paused(&session),
                    resume_at: self.sessions.queue_resume_at(&session),
                })
            }
            Request::QueueMessage { session, text } => {
                self.session(&session)?;
                if self.sessions.would_wait(&session) {
                    let text = self.expand_attachments(&text);
                    self.sessions.enqueue(&session, text).map_err(failed)?;
                    Ok(Response::Ack)
                } else {
                    // Nothing to wait behind: a queue of one is a send.
                    self.send_message(&session, text)
                }
            }
            Request::InterruptWithQueuedMessage { session, id } => {
                self.session(&session)?;
                let stopped = self
                    .sessions
                    .interrupt_with_queued(&session, id)
                    .map_err(failed)?;
                if !stopped {
                    self.dispatch_front(&session)?;
                }
                Ok(Response::Ack)
            }
            Request::SetQueuePaused { session, paused } => {
                self.session(&session)?;
                if self.sessions.set_queue_paused(&session, paused) {
                    self.dispatch_front(&session)?;
                }
                Ok(Response::Ack)
            }
            Request::ClearQueue { session } => {
                self.session(&session)?;
                self.sessions.clear_queue(&session);
                Ok(Response::Ack)
            }
            Request::EditQueuedMessage { session, id, text } => {
                self.session(&session)?;
                self.sessions
                    .edit_queued_message(&session, id, text)
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::RemoveQueuedMessage { session, id } => {
                self.session(&session)?;
                self.sessions
                    .remove_queued_message(&session, id)
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::MoveQueuedMessage { session, id, index } => {
                self.session(&session)?;
                self.sessions
                    .move_queued_message(&session, id, index as usize)
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::SendQueuedMessageNow { session, id } => {
                self.session(&session)?;
                self.sessions
                    .send_queued_message_now(&session, id)
                    .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::CompactSession { session } => self.compact_session(&session),
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
            } => self.fork_session(&session, after, agent.as_deref(), model, account, false),
            Request::EditPrompt { session, seq, text } => self.edit_prompt(&session, seq, text),
            Request::CliSessions { workspace } => {
                let worktree = self.worktree(&workspace)?;
                let known = session::vendor_ids(&self.conn()).map_err(failed)?;
                let roots = self.cli_roots();
                Ok(Response::CliSessions {
                    sessions: crate::cli_sessions::list(
                        &mut self.cli_index,
                        &roots,
                        &worktree.path,
                        &known,
                    )
                    .into_iter()
                    .map(|found| found.session)
                    .collect(),
                })
            }
            Request::AdoptCliSession {
                workspace,
                agent,
                vendor_session_id,
            } => self.adopt_cli_session(&workspace, &agent, &vendor_session_id),
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
            Request::SessionTranscriptTail {
                session,
                before,
                limit,
            } => {
                self.session(&session)?;
                Ok(Response::Transcript {
                    entries: session::transcript_tail(
                        &self.conn(),
                        &session,
                        before,
                        limit.min(5_000),
                    )
                    .map_err(failed)?,
                })
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

            Request::WorkspaceChanges {
                workspace,
                source,
                context_lines,
            } => {
                let context_lines = context_lines.unwrap_or(3);
                if context_lines > 25 {
                    return Err(RpcError::failed("diff context exceeds 25 lines"));
                }
                let worktree = self.worktree(&workspace)?;
                let files = match &source {
                    // A checkpoint names a commit, and the commit is what git
                    // can be asked about.
                    ChangeSource::SinceCheckpoint { checkpoint } => {
                        let stored = checkpoint::get(&self.conn(), checkpoint)
                            .map_err(failed)?
                            .filter(|stored| stored.workspace == workspace)
                            .ok_or_else(|| {
                                RpcError::not_found(format!(
                                    "no checkpoint with id {} in workspace {}",
                                    checkpoint.0, workspace.0
                                ))
                            })?;
                        git::changes_since_with_context(
                            &worktree.path,
                            &stored.commit,
                            context_lines,
                        )
                        .map_err(failed)?
                    }
                    ChangeSource::Turn { checkpoint } => {
                        let conn = self.conn();
                        let stored = checkpoint::get(&conn, checkpoint)
                            .map_err(failed)?
                            .filter(|stored| stored.workspace == workspace)
                            .ok_or_else(|| {
                                RpcError::not_found(format!(
                                    "no checkpoint with id {} in workspace {}",
                                    checkpoint.0, workspace.0
                                ))
                            })?;
                        match checkpoint::turn_commits(&conn, &stored.id).map_err(failed)? {
                            Some((start, end)) => git::changes_between_with_context(
                                &worktree.path,
                                &start,
                                &end,
                                context_lines,
                            )
                            .map_err(failed)?,
                            None => {
                                return Err(RpcError::not_found(format!(
                                    "checkpoint {} has no saved turn start",
                                    checkpoint.0
                                )));
                            }
                        }
                    }
                    ChangeSource::Branch { base } => {
                        let base = match base {
                            Some(base) => base.clone(),
                            None => self.project(&worktree.project)?.default_branch,
                        };
                        let fork = git::fork_point(&worktree.path, &base).map_err(failed)?;
                        git::changes_since_with_context(&worktree.path, &fork, context_lines)
                            .map_err(failed)?
                    }
                    other => git::changes_with_context(&worktree.path, other, context_lines)
                        .map_err(failed)?,
                };
                let mut files = files;
                // Orca's line attribution, for diffs of the worktree: which
                // added lines the turns since the last commit wrote.
                if matches!(source, ChangeSource::Uncommitted | ChangeSource::Unstaged) {
                    self.attribute(&workspace, &worktree.path, &mut files);
                }
                Ok(Response::Changes {
                    changes: Changes { source, files },
                })
            }
            Request::WorkspaceHistory { workspace, limit } => {
                let worktree = self.worktree(&workspace)?;
                let limit = limit.unwrap_or(50).min(200) as usize;
                let commits = git::history(&worktree.path, limit).map_err(failed)?;
                Ok(Response::History { commits })
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
            Request::StageHunk {
                workspace,
                path,
                header,
                staged,
            } => {
                let worktree = self.worktree(&workspace)?;
                git::stage_hunk(&worktree.path, &path, &header, staged).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::RevertHunk {
                workspace,
                path,
                header,
            } => {
                let worktree = self.worktree(&workspace)?;
                git::revert_hunk(&worktree.path, &path, &header).map_err(failed)?;
                self.events.emit(DaemonEvent::WorkspacesChanged {
                    project: worktree.project.clone(),
                });
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
            Request::ListNotes {
                project,
                query,
                tag,
            } => Ok(Response::Notes {
                notes: crate::notes::list(
                    &self.conn(),
                    project.as_ref(),
                    query.as_deref(),
                    tag.as_deref(),
                )
                .map_err(failed)?,
            }),
            Request::SaveNote {
                id,
                project,
                title,
                body,
                tags,
            } => Ok(Response::Note {
                note: crate::notes::save(
                    &self.conn(),
                    id.as_deref(),
                    project.as_ref(),
                    &title,
                    &body,
                    tags.as_deref(),
                    now(),
                )
                .map_err(failed)?,
            }),
            Request::ListQuickCommands { project } => Ok(Response::QuickCommands {
                commands: crate::quick_commands::list(&self.conn(), project.as_ref())
                    .map_err(failed)?,
            }),
            Request::SaveQuickCommand {
                id,
                project,
                name,
                kind,
                body,
            } => Ok(Response::QuickCommand {
                command: crate::quick_commands::save(
                    &self.conn(),
                    id.as_deref(),
                    project.as_ref(),
                    &name,
                    kind,
                    &body,
                )
                .map_err(failed)?,
            }),
            Request::RemoveQuickCommand { id } => {
                crate::quick_commands::remove(&self.conn(), &id).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::RunQuickCommand {
                workspace,
                id,
                rows,
                cols,
            } => {
                let worktree = self.worktree(&workspace)?;
                let command = crate::quick_commands::get(&self.conn(), &id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::not_found(format!("no quick command with id {id}")))?;
                if command.kind != ginka_protocol::model::QuickCommandKind::Shell {
                    return Err(RpcError::failed(
                        "a prompt is sent to the conversation, not run in a terminal",
                    ));
                }
                let terminal = self
                    .terminals
                    .open_command(
                        &workspace,
                        &worktree.path,
                        crate::terminal::TerminalCommand {
                            program: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                            args: crate::quick_commands::shell_args(&command.body),
                            env: Vec::new(),
                            title: command.name.clone(),
                            on_exit: None,
                        },
                        rows,
                        cols,
                    )
                    .map_err(failed)?;
                Ok(Response::Terminal { terminal })
            }
            Request::DaemonSettings => {
                let mut value = serde_json::to_value(&self.settings).map_err(failed)?;
                redact_env(&mut value);
                Ok(Response::DaemonSettings {
                    json: serde_json::to_string_pretty(&value).map_err(failed)?,
                })
            }
            Request::ListProviderSettings => Ok(Response::ProviderSettings {
                providers: Registry::with_defaults()
                    .ids()
                    .into_iter()
                    .filter_map(ProviderKind::parse)
                    .map(|provider| ProviderSetting {
                        provider,
                        enabled: self.settings.is_enabled(provider),
                        program: self
                            .settings
                            .binary_override(provider)
                            .map(|path| path.to_string_lossy().into_owned()),
                    })
                    .collect(),
            }),
            Request::UpdateProviderSettings {
                provider,
                enabled,
                program,
                clear_program,
            } => {
                if program.is_some() && clear_program {
                    return Err(RpcError::failed("program and clear_program conflict"));
                }
                if program
                    .as_ref()
                    .is_some_and(|program| program.trim().is_empty())
                {
                    return Err(RpcError::failed("program must not be empty"));
                }
                if !Registry::with_defaults().ids().contains(&provider.as_str()) {
                    return Err(RpcError::failed(format!(
                        "{} is not a shipped provider",
                        provider.as_str()
                    )));
                }
                let mut settings = self.settings.clone();
                if let Some(enabled) = enabled {
                    settings.set_enabled(provider, enabled);
                }
                if clear_program {
                    settings.set_binary_override(provider, None);
                } else if let Some(program) = program {
                    settings.set_binary_override(provider, Some(std::path::PathBuf::from(program)));
                }
                crate::settings::save(&self.paths.daemon_settings(), &settings).map_err(failed)?;
                self.take_settings(settings);
                Ok(Response::Ack)
            }
            Request::UpdateDaemonSettings { key, value } => {
                let value: serde_json::Value = serde_json::from_str(&value).map_err(|error| {
                    RpcError::failed(format!("{key}: the value is not JSON ({error})"))
                })?;
                let mut current = serde_json::to_value(&self.settings).map_err(failed)?;
                let fields = current
                    .as_object_mut()
                    .ok_or_else(|| RpcError::failed("the settings are not an object"))?;
                if !fields.contains_key(&key) {
                    return Err(RpcError::failed(format!(
                        "there is no setting called {key}"
                    )));
                }
                fields.insert(key.clone(), value);
                let settings: crate::settings::DaemonSettings = serde_json::from_value(current)
                    .map_err(|error| RpcError::failed(format!("{key}: {error}")))?;
                crate::settings::save(&self.paths.daemon_settings(), &settings).map_err(failed)?;
                self.take_settings(settings);
                Ok(Response::Ack)
            }
            Request::RecordBrowserVisit {
                workspace,
                url,
                title,
            } => {
                crate::browser_history::record(
                    &self.conn(),
                    &workspace,
                    &url,
                    title.as_deref(),
                    now(),
                )
                .map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::BrowserSuggestions {
                workspace,
                query,
                limit,
            } => {
                let pages = crate::browser_history::suggestions(
                    &self.conn(),
                    &workspace,
                    &query,
                    now(),
                    limit.unwrap_or(8).min(50) as usize,
                )
                .map_err(failed)?
                .into_iter()
                .map(|visited| ginka_protocol::model::VisitedPage {
                    url: visited.url,
                    title: visited.title,
                    visits: visited.visits,
                    last_visited_at: visited.last_visited_at,
                })
                .collect();
                Ok(Response::BrowserSuggestions { pages })
            }
            Request::InstallBundledSkills { force } => {
                let home = self
                    .home()
                    .ok_or_else(|| RpcError::failed("the daemon has no home directory"))?;
                let mut results = Vec::new();
                // The two whose agents Ginka runs, and whose skills directory
                // each of them reads.
                for relative in [".claude/skills", ".codex/skills"] {
                    let root = home.join(relative);
                    for (name, outcome) in
                        crate::skills::install_bundled(&root, force).map_err(failed)?
                    {
                        results.push(ginka_protocol::model::BundledSkillInstall {
                            name: name.to_string(),
                            path: root
                                .join(name)
                                .join(crate::skills::SKILL_FILE)
                                .display()
                                .to_string(),
                            outcome: match outcome {
                                crate::skills::Installed::Written => "written",
                                crate::skills::Installed::Unchanged => "unchanged",
                                crate::skills::Installed::Kept => "kept",
                            }
                            .to_string(),
                        });
                    }
                }
                Ok(Response::BundledSkillsInstalled { results })
            }
            Request::ListCronJobs { project } => Ok(Response::CronJobs {
                jobs: crate::cron::list(&self.conn(), project.as_ref()).map_err(failed)?,
            }),
            Request::SaveCronJob {
                id,
                project,
                workspace,
                session,
                name,
                schedule,
                via,
                agent,
                body,
                precheck,
                enabled,
            } => {
                self.project(&project)?;
                if let Some(workspace) = &workspace
                    && self.worktree(workspace)?.project != project
                {
                    return Err(RpcError::failed(format!(
                        "{workspace} is not a workspace of {project}"
                    )));
                }
                // A reminder continues a conversation of this project, on the
                // agent that conversation already has.
                let (workspace, agent) = match &session {
                    Some(target) => {
                        let stored = self.session(target)?;
                        if stored.workspace.parts().map(|(owner, _)| owner) != Some(project.clone())
                        {
                            return Err(RpcError::failed(format!(
                                "conversation {target} is not in {project}"
                            )));
                        }
                        (Some(stored.workspace), Some(stored.agent))
                    }
                    None => (workspace, agent),
                };
                if let Some(agent) = &agent
                    && via == ginka_protocol::model::CronVia::Chat
                    && self.drivers.get(agent).is_none()
                {
                    return Err(RpcError::failed(format!("no agent named {agent}")));
                }
                let draft = crate::cron::Draft {
                    project,
                    workspace,
                    session,
                    name,
                    schedule,
                    via,
                    agent,
                    body,
                    precheck,
                    enabled,
                };
                let job =
                    crate::cron::save(&self.conn(), id, &draft, chrono::Utc::now().timestamp())
                        .map_err(failed)?;
                Ok(Response::CronJob { job })
            }
            Request::RemoveCronJob { id } => {
                crate::cron::remove(&self.conn(), id).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::RunCronJob { id } => {
                let job = crate::cron::get(&self.conn(), id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::not_found(format!("no scheduled job with id {id}")))?;
                self.fire_cron(&job);
                let job = crate::cron::get(&self.conn(), id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::not_found(format!("no scheduled job with id {id}")))?;
                Ok(Response::CronJob { job })
            }
            Request::CronRuns { id, limit } => Ok(Response::CronRuns {
                runs: crate::cron::runs(&self.conn(), id, limit.unwrap_or(50).min(500))
                    .map_err(failed)?,
            }),
            Request::RemoveNote { id } => {
                crate::notes::remove(&self.conn(), &id).map_err(failed)?;
                Ok(Response::Ack)
            }
            Request::RaiseTicket {
                workspace,
                from_session,
                title,
                summary,
                prompt,
            } => self.raise_ticket(workspace, from_session, &title, &summary, &prompt),
            Request::ListTickets { workspace, all } => Ok(Response::Tickets {
                tickets: crate::tickets::list(&self.conn(), workspace.as_ref(), all)
                    .map_err(failed)?,
            }),
            Request::StartTicket {
                ticket,
                agent,
                branch,
            } => self.start_ticket(&ticket, agent, branch),
            Request::DismissTicket { ticket } => {
                let ticket = crate::tickets::close(
                    &self.conn(),
                    &ticket,
                    ginka_protocol::model::TicketState::Dismissed,
                    None,
                    now(),
                )
                .map_err(failed)?;
                self.events.emit(DaemonEvent::TicketsChanged {
                    workspace: ticket.workspace,
                });
                Ok(Response::Ack)
            }
            Request::MessageSession { from, to, text } => self.message_session(&from, &to, &text),
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
            Request::CreateGeneratedPullRequest {
                workspace,
                draft,
                agent,
            } => self.create_generated_pull_request(workspace, agent.as_deref(), draft),
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
            Request::CreateSkill {
                name,
                description,
                body,
                project,
            } => {
                let base = match project {
                    Some(project) => self.project(&project)?.path,
                    None => self
                        .home()
                        .ok_or_else(|| RpcError::not_found("home directory unavailable"))?,
                };
                crate::skills::create(&base.join(".agents/skills"), &name, &description, &body)
                    .map_err(failed)?;
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
                // Read before the connection is held: it asks the database too.
                let folders = self.project_folders();
                let conn = self.conn();
                let report = crate::usage::report(&conn, days, self.rates.as_ref(), &folders)
                    .map_err(failed)?;
                Ok(Response::Usage {
                    by_day: report.by_day,
                    by_agent: report.by_agent,
                    by_account: report.by_account,
                    plans: crate::usage::plans(&conn).map_err(failed)?,
                    rates_fetched_at: self.rates.as_ref().map(|rates| rates.fetched_at),
                    by_model: report.by_model,
                    by_project: report.by_project,
                })
            }
            Request::AddReviewComment {
                workspace,
                path,
                line,
                end_line,
                side,
                text,
            } => {
                self.worktree(&workspace)?;
                if text.trim().is_empty() {
                    return Err(RpcError::failed(
                        "a comment with nothing in it says nothing",
                    ));
                }
                crate::comments::add(
                    &self.conn(),
                    &workspace,
                    &path,
                    line,
                    end_line,
                    side,
                    &text,
                    now(),
                )
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
            Request::ResolveConflicts { workspace, agent } => {
                let worktree = self.worktree(&workspace)?;
                let conflicts = crate::conflicts::read(&worktree.path).map_err(failed)?;
                if conflicts.paths.is_empty() {
                    return Err(RpcError::failed(format!(
                        "nothing is conflicted in {workspace}"
                    )));
                }
                let prompt = crate::conflicts::prompt(&conflicts);
                self.hand_to_agent(workspace, agent, prompt)
            }
            Request::ImageDiff {
                workspace,
                source,
                path,
                old_path,
            } => {
                let worktree = self.worktree(&workspace)?;
                let old_path = old_path.as_deref();
                // Sources that name a checkpoint or a base are resolved to
                // commits here, where the database and the project are.
                let between = |from: &str, to: Option<&str>| {
                    crate::image_diff::sides_between(&worktree.path, from, to, &path, old_path)
                        .map_err(failed)
                };
                let (before, after) = match &source {
                    ChangeSource::Turn { checkpoint } => {
                        let pair = checkpoint::turn_commits(&self.conn(), checkpoint)
                            .map_err(failed)?
                            .ok_or_else(|| {
                                RpcError::not_found(format!(
                                    "checkpoint {} has no saved turn start",
                                    checkpoint.0
                                ))
                            })?;
                        between(&pair.0, Some(&pair.1))?
                    }
                    ChangeSource::SinceCheckpoint { checkpoint } => {
                        let stored = checkpoint::get(&self.conn(), checkpoint)
                            .map_err(failed)?
                            .filter(|stored| stored.workspace == workspace)
                            .ok_or_else(|| {
                                RpcError::not_found(format!(
                                    "no checkpoint with id {}",
                                    checkpoint.0
                                ))
                            })?;
                        between(&stored.commit, None)?
                    }
                    ChangeSource::Branch { base } => {
                        let base = match base {
                            Some(base) => base.clone(),
                            None => self.project(&worktree.project)?.default_branch,
                        };
                        let fork = git::fork_point(&worktree.path, &base).map_err(failed)?;
                        between(&fork, None)?
                    }
                    other => crate::image_diff::sides(&worktree.path, other, &path, old_path)
                        .map_err(failed)?,
                };
                Ok(Response::ImageDiff { before, after })
            }
            Request::ListMcpServers { workspace } => {
                let project = match &workspace {
                    Some(workspace) => Some(self.worktree(workspace)?.path),
                    None => None,
                };
                let home = dirs::home_dir()
                    .ok_or_else(|| RpcError::failed("there is no home directory to read"))?;
                Ok(Response::McpServers {
                    servers: crate::mcp_inventory::discover(&home, project.as_deref()),
                })
            }
            Request::AddMcpServer { workspace, spec } => {
                let args = crate::mcp_inventory::add_args(&spec).map_err(RpcError::failed)?;
                self.run_vendor_mcp(&spec.provider, workspace.as_ref(), spec.scope, &args)?;
                Ok(Response::Ack)
            }
            Request::RemoveMcpServer {
                workspace,
                provider,
                name,
                scope,
            } => {
                let args = crate::mcp_inventory::remove_args(&provider, &name, scope)
                    .map_err(RpcError::failed)?;
                self.run_vendor_mcp(
                    &provider,
                    workspace.as_ref(),
                    scope.unwrap_or(ginka_protocol::model::McpScope::User),
                    &args,
                )?;
                Ok(Response::Ack)
            }
            Request::FixCommitFailure {
                workspace,
                message,
                output,
                agent,
            } => {
                let worktree = self.worktree(&workspace)?;
                if output.trim().is_empty() {
                    return Err(RpcError::failed("there is no failure to hand over"));
                }
                let staged = crate::commit_failure::staged_paths(&worktree.path).map_err(failed)?;
                let prompt = crate::commit_failure::prompt(&message, &output, &staged);
                self.hand_to_agent(workspace, agent, prompt)
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
            Request::ReadAttachmentImage { reference } => {
                let store = crate::attachment::AttachmentStore::new(self.paths.attachments());
                let image = store
                    .path_of(&reference)
                    .and_then(|path| std::fs::symlink_metadata(path).ok())
                    .filter(|metadata| {
                        metadata.file_type().is_file()
                            && metadata.len() <= crate::files::IMAGE_PREVIEW_LIMIT as u64
                    })
                    .and_then(|_| store.read(&reference).ok().flatten())
                    .and_then(|bytes| crate::files::preview_image(&bytes));
                Ok(Response::AttachmentImage { image })
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
            Request::ReadFile { workspace, path } => {
                let worktree = self.worktree(&workspace)?;
                Ok(Response::FileContent {
                    file: crate::files::read(&worktree.path, &path).map_err(failed)?,
                })
            }
            Request::OpenExternalEditor {
                workspace,
                path,
                line,
            } => {
                let worktree = self.worktree(&workspace)?;
                crate::external_editor::open(&worktree.path, &path, line).map_err(failed)?;
                Ok(Response::Ack)
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
                if let Some((signed_in, identity)) =
                    crate::driver::probe::probe_login(driver.as_ref(), &env)
                {
                    account.signed_in = Some(signed_in);
                    account.identity = identity;
                }
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
        use crate::conversation_commands::Command;
        // `/plan` on a new conversation: the conversation keeps the mode it
        // was asked for, and only this first turn runs read-only.
        let mut first_turn = None;
        let mut prompt = prompt;
        let goal = match crate::conversation_commands::parse(&prompt) {
            Some(Command::Plan(request)) => {
                if request.is_empty() {
                    return Err(RpcError::failed("/plan needs something to plan"));
                }
                prompt = request.to_owned();
                first_turn = Some(AccessMode::ReadOnly);
                None
            }
            Some(Command::Goal(objective)) if !objective.is_empty() => Some(objective.to_owned()),
            Some(Command::Side(_) | Command::Btw(_)) => {
                return Err(RpcError::failed(
                    "/side and /btw need an existing conversation",
                ));
            }
            Some(Command::ClearGoal) => {
                return Err(RpcError::failed("/goal done needs an existing goal"));
            }
            Some(Command::Goal(_)) => return Err(RpcError::failed("/goal needs an objective")),
            None => None,
        };
        let prompt = goal.clone().unwrap_or(prompt);
        let preamble = match (&goal, first_turn) {
            (Some(objective), _) => Some(goal_preamble(objective)),
            (None, Some(_)) => Some(crate::conversation_commands::PLAN_INSTRUCTION.to_string()),
            (None, None) => None,
        };
        self.start_session_with_context(
            workspace,
            agent,
            prompt,
            model,
            reasoning_effort,
            service_tier,
            account,
            access_mode,
            origin,
            preamble,
            goal,
            first_turn,
        )
    }

    /// Start a fresh vendor thread with optional context kept outside its transcript.
    #[allow(clippy::too_many_arguments)]
    fn start_session_with_context(
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
        preamble: Option<String>,
        goal: Option<String>,
        first_turn: Option<AccessMode>,
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
        if let Some(objective) = goal {
            crate::conversation_commands::set_goal(&self.conn(), &session.id, Some(&objective))
                .map_err(failed)?;
        }
        self.events.emit(DaemonEvent::SessionStarted {
            session: Box::new(session.clone()),
        });

        // The agent reads files, not URIs: an attachment the user mentioned
        // reaches it as a path on this host (§3.3 N6). The transcript keeps
        // the reference, so the window can still draw the attachment.
        let mut spec = SessionSpec::new(&worktree.path, self.expand_attachments(&prompt))
            .with_preamble(preamble)
            .with_model(model)
            .with_reasoning_effort(reasoning_effort)
            .with_service_tier(service_tier)
            // A narrower first turn (`/plan`) never widens what was asked.
            .with_access_mode(
                first_turn
                    .filter(|mode| mode.is_narrower_than(access_mode))
                    .unwrap_or(access_mode),
            )
            .with_mcp_servers(self.mcp_servers(&worktree.path, &session.id));
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
            if let Some(objective) =
                crate::conversation_commands::goal(&tx, &original.id).map_err(failed)?
            {
                crate::conversation_commands::set_goal(&tx, &replacement.id, Some(&objective))
                    .map_err(failed)?;
            }
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
        fresh_thread: bool,
    ) -> Result<Response, RpcError> {
        let original = self.session(id)?;
        let driver = self.driver(agent.unwrap_or(&original.agent))?;
        let same_agent = driver.id() == original.agent;
        let account = match account {
            Some(account) => crate::account::resolve(&self.settings, driver.id(), Some(&account))
                .map_err(account_error)?,
            // No login named: the original's where the agent is the same,
            // the provider's active account where it is not — the original's
            // login belongs to another provider.
            None if same_agent => original.account.clone(),
            None => {
                crate::account::resolve(&self.settings, driver.id(), None).map_err(account_error)?
            }
        };
        // A fresh thread is a move in all but name: the vendor's thread holds
        // what is being replaced, so the digest stands in for it.
        let moved = !same_agent || account != original.account || fresh_thread;
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
                // An edited prompt is the same conversation taken another way.
                (Some(title), _) if fresh_thread => title.clone(),
                (None, _) if fresh_thread => "edited".to_string(),
                (Some(title), false) => format!("{title} (fork)"),
                (Some(title), true) => format!("{title} ({} fork)", driver.id()),
                (None, false) => "fork".to_string(),
                (None, true) => format!("{} fork", driver.id()),
            }),
            summary: (moved && !fresh_thread).then(|| format!("moved from {}", original.agent)),
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
            if let Some(objective) =
                crate::conversation_commands::goal(&conn, id).map_err(failed)?
            {
                crate::conversation_commands::set_goal(&conn, &fork.id, Some(&objective))
                    .map_err(failed)?;
            }
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

    /// Where the CLIs keep their conversations for this service.
    fn cli_roots(&self) -> crate::cli_sessions::Roots {
        self.cli_roots
            .clone()
            .unwrap_or_else(crate::cli_sessions::Roots::from_env)
    }

    /// Make a session of a conversation an agent's CLI started here.
    fn adopt_cli_session(
        &mut self,
        workspace: &WorkspaceId,
        agent: &str,
        vendor_id: &str,
    ) -> Result<Response, RpcError> {
        let worktree = self.worktree(workspace)?;
        let driver = self.driver(agent)?;
        if session::vendor_ids(&self.conn())
            .map_err(failed)?
            .contains(vendor_id)
        {
            return Err(RpcError::failed(format!(
                "a session already holds {agent} conversation {vendor_id}"
            )));
        }
        let roots = self.cli_roots();
        let found = crate::cli_sessions::find(
            &mut self.cli_index,
            &roots,
            &worktree.path,
            agent,
            vendor_id,
        )
        .ok_or_else(|| {
            RpcError::not_found(format!(
                "no {agent} conversation {vendor_id} was started in {}",
                worktree.path.display()
            ))
        })?;
        let imported = crate::cli_sessions::transcript(&found).map_err(failed)?;
        let now = now();
        let adopted = Session {
            id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
            workspace: workspace.clone(),
            agent: driver.id().to_string(),
            // The CLI wrote it under the system login, and only that login
            // can resume it.
            account: ginka_protocol::AccountId(driver.id().to_string()),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            access_mode: AccessMode::default(),
            origin: None,
            state: SessionState::Idle,
            title: Some(found.session.title.clone()).filter(|title| !title.is_empty()),
            summary: Some(format!("resumed from the {} CLI", driver.id())),
            vendor_session_id: Some(found.session.vendor_session_id.clone()),
            created_at: now,
            updated_at: now,
        };
        {
            let conn = self.conn();
            session::insert(&conn, &adopted).map_err(failed)?;
            for (at, payload) in &imported {
                // An entry the vendor did not date is placed now.
                let at = if *at > 0 { *at } else { now };
                session::append(&conn, &adopted.id, payload, at).map_err(failed)?;
            }
            session::touch(&conn, &adopted.id, now).map_err(failed)?;
        }
        self.events.emit(DaemonEvent::SessionStarted {
            session: Box::new(adopted.clone()),
        });
        if let Some((project, _)) = workspace.parts() {
            self.events.emit(DaemonEvent::WorkspacesChanged { project });
        }
        Ok(Response::Session { session: adopted })
    }

    /// Edit a sent prompt and run the conversation again from it.
    fn edit_prompt(
        &mut self,
        id: &SessionId,
        seq: u64,
        text: String,
    ) -> Result<Response, RpcError> {
        if text.trim().is_empty() {
            return Err(RpcError::failed("an edited prompt cannot be blank"));
        }
        let original = self.session(id)?;
        if self.sessions.is_running(id) {
            return Err(RpcError::failed(
                "the session is still working; stop it before editing a prompt",
            ));
        }
        let entries = session::transcript(&self.conn(), id, None, None).map_err(failed)?;
        let Some(prompt) = entries.iter().find(|entry| entry.seq == seq) else {
            return Err(RpcError::not_found(format!("no transcript entry {seq}")));
        };
        if !matches!(prompt.payload, TranscriptPayload::User { .. }) {
            return Err(RpcError::failed(format!("entry {seq} is not a prompt")));
        }
        // The turns finished before this prompt: the checkpoint at the end of
        // the last of them (or turn 0, taken before the first) is the state
        // the prompt was sent into.
        let finished = entries
            .iter()
            .filter(|entry| entry.seq < seq)
            .filter(|entry| {
                matches!(
                    entry.payload,
                    TranscriptPayload::Agent {
                        event: AgentEvent::TurnEnd { .. }
                    }
                )
            })
            .count() as u32;
        let before = checkpoint::list(&self.conn(), &original.workspace)
            .map_err(failed)?
            .into_iter()
            .filter(|checkpoint| &checkpoint.session == id && checkpoint.turn == finished)
            // The one the turn took, not a later "before restoring" snapshot.
            .min_by_key(|checkpoint| checkpoint.created_at);
        if let Some(checkpoint) = before {
            self.restore(&checkpoint.id)?;
        }
        let fork =
            match self.fork_session(id, Some(seq.saturating_sub(1)), None, None, None, true)? {
                Response::Session { session } => session,
                other => {
                    return Err(RpcError::failed(format!(
                        "unexpected fork answer: {other:?}"
                    )));
                }
            };
        self.send_message(&fork.id, text)?;
        Ok(Response::Session { session: fork })
    }

    /// Send a follow-up to a session, queued if its agent is still working.
    fn send_message(&mut self, id: &SessionId, text: String) -> Result<Response, RpcError> {
        use crate::conversation_commands::{self as conversation, Command};
        let stored = self.session(id)?;
        let command = conversation::parse(&text);
        match command {
            Some(Command::ClearGoal) => {
                conversation::set_goal(&self.conn(), id, None).map_err(failed)?;
                return Ok(Response::Ack);
            }
            Some(Command::Goal(objective)) => {
                if objective.is_empty() {
                    return Err(RpcError::failed("/goal needs an objective"));
                }
                conversation::set_goal(&self.conn(), id, Some(objective)).map_err(failed)?;
                return self.send_plain_message(id, objective.to_owned(), stored);
            }
            Some(Command::Plan(request)) => {
                if request.is_empty() {
                    return Err(RpcError::failed("/plan needs something to plan"));
                }
                // Narrowing for one turn: the stored mode is not touched, so
                // the next message runs as the conversation always has.
                return self.send_turn(
                    id,
                    request.to_owned(),
                    stored,
                    Some(AccessMode::ReadOnly),
                    Some(conversation::PLAN_INSTRUCTION),
                );
            }
            Some(Command::Side(question) | Command::Btw(question)) => {
                let is_btw = matches!(command, Some(Command::Btw(_)));
                if question.is_empty() && is_btw {
                    return Err(RpcError::failed("/btw needs a question"));
                }
                let entries = session::transcript(&self.conn(), id, None, None).map_err(failed)?;
                let digest =
                    crate::handoff::digest(&entries, &stored.agent, crate::handoff::DEFAULT_BUDGET);
                let instruction = if is_btw {
                    "Answer this brief question using the main conversation as context. Do not change files or carry your answer back into the main conversation."
                } else {
                    "This is a connected side conversation. Use the main conversation as context, but keep this conversation's messages separate."
                };
                let preamble = format!("{instruction}\n\n{digest}");
                if question.is_empty() {
                    let now = now();
                    let side = Session {
                        id: SessionId(uuid::Uuid::new_v4().simple().to_string()),
                        workspace: stored.workspace.clone(),
                        agent: stored.agent.clone(),
                        account: stored.account.clone(),
                        model: stored.model.clone(),
                        reasoning_effort: stored.reasoning_effort.clone(),
                        service_tier: stored.service_tier.clone(),
                        state: SessionState::Idle,
                        title: Some("Side chat".into()),
                        summary: None,
                        vendor_session_id: None,
                        access_mode: stored.access_mode,
                        origin: None,
                        created_at: now,
                        updated_at: now,
                    };
                    let conn = self.conn();
                    session::insert(&conn, &side).map_err(failed)?;
                    session::set_handoff(&conn, &side.id, &preamble).map_err(failed)?;
                    conversation::link_side(&conn, &side.id, id, "side").map_err(failed)?;
                    self.events.emit(DaemonEvent::SessionStarted {
                        session: Box::new(side.clone()),
                    });
                    return Ok(Response::Session { session: side });
                }
                let result = self.start_session_with_context(
                    stored.workspace.clone(),
                    &stored.agent,
                    question.to_owned(),
                    stored.model.clone(),
                    stored.reasoning_effort.clone(),
                    stored.service_tier.clone(),
                    Some(stored.account.clone()),
                    if is_btw {
                        AccessMode::ReadOnly
                    } else {
                        stored.access_mode
                    },
                    None,
                    Some(preamble),
                    None,
                    None,
                )?;
                if let Response::Session { session } = &result {
                    conversation::link_side(
                        &self.conn(),
                        &session.id,
                        id,
                        if is_btw { "btw" } else { "side" },
                    )
                    .map_err(failed)?;
                }
                return Ok(result);
            }
            None => {}
        }
        self.send_plain_message(id, text, stored)
    }

    /// Send ordinary text after any Ginka-owned command has been handled.
    fn send_plain_message(
        &mut self,
        id: &SessionId,
        text: String,
        stored: Session,
    ) -> Result<Response, RpcError> {
        self.send_turn(id, text, stored, None, None)
    }

    /// Send a turn, optionally narrowed to `mode` and with `instruction`
    /// ahead of the prompt — for this turn only. A wider mode than the
    /// conversation's is never used: widening is the restart N2 reserves.
    fn send_turn(
        &mut self,
        id: &SessionId,
        text: String,
        stored: Session,
        mode: Option<AccessMode>,
        instruction: Option<&str>,
    ) -> Result<Response, RpcError> {
        use crate::conversation_commands as conversation;
        let worktree = self.worktree(&stored.workspace)?;
        let driver = self.driver(&stored.agent)?;
        // What a conversation moved from another agent still owes it: the
        // digest rides in front of the first prompt, once, and only while
        // the agent has no thread of its own to have read it in.
        let mut preamble = match stored.vendor_session_id {
            None => session::handoff(&self.conn(), id).map_err(failed)?,
            Some(_) => None,
        };
        if let Some(objective) = conversation::goal(&self.conn(), id).map_err(failed)? {
            let instruction = goal_preamble(&objective);
            preamble = Some(match preamble {
                Some(prior) => format!("{prior}\n\n{instruction}"),
                None => instruction,
            });
        }
        if let Some(instruction) = instruction {
            preamble = Some(match preamble {
                Some(prior) => format!("{prior}\n\n{instruction}"),
                None => instruction.to_string(),
            });
        }
        let mode = match mode {
            Some(narrower) if narrower.is_narrower_than(stored.access_mode) => narrower,
            _ => stored.access_mode,
        };
        let mut spec = SessionSpec::new(&worktree.path, self.expand_attachments(&text))
            .with_model(stored.model.clone())
            .with_reasoning_effort(stored.reasoning_effort.clone())
            .with_service_tier(stored.service_tier.clone())
            // The mode the conversation was started in, or a narrower one
            // for this turn: a resume that widened it would be the change N2
            // reserves for a new session.
            .with_access_mode(mode)
            .with_preamble(preamble)
            .with_mcp_servers(self.mcp_servers(&worktree.path, id));
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
        if stored.vendor_session_id.is_none()
            && session::transcript(&self.conn(), id, None, Some(1))
                .map_err(failed)?
                .is_empty()
        {
            self.sessions
                .start(id.clone(), driver, spec)
                .map_err(failed)?;
        } else {
            self.sessions
                .send(id.clone(), driver, spec, stored.vendor_session_id)
                .map_err(failed)?;
        }
        Ok(Response::Ack)
    }

    /// Record a ticket and tell every window, so the card appears while the
    /// agent that raised it is still working.
    fn raise_ticket(
        &mut self,
        workspace: Option<WorkspaceId>,
        from_session: Option<SessionId>,
        title: &str,
        summary: &str,
        prompt: &str,
    ) -> Result<Response, RpcError> {
        let from = from_session
            .as_ref()
            .map(|id| self.session(id))
            .transpose()?;
        let workspace = match (workspace, &from) {
            (Some(workspace), _) => workspace,
            (None, Some(from)) => from.workspace.clone(),
            (None, None) => {
                return Err(RpcError::failed(
                    "a ticket needs a workspace, or a session to take it from",
                ));
            }
        };
        self.worktree(&workspace)?;
        let ticket = crate::tickets::raise(
            &self.conn(),
            crate::tickets::NewTicket {
                workspace: &workspace,
                from_session: from_session.as_ref(),
                title,
                summary,
                prompt,
            },
            now(),
        )
        .map_err(failed)?;
        self.events.emit(DaemonEvent::TicketsChanged { workspace });
        Ok(Response::Ticket { ticket })
    }

    /// Start a session from an open ticket.
    ///
    /// The agent is the raising session's unless one is named, so a ticket a
    /// Claude session raised is picked up by Claude; a ticket raised from the
    /// command line falls back to the first agent on offer. With a branch the
    /// work gets its own worktree, cut from the project's default branch.
    fn start_ticket(
        &mut self,
        id: &str,
        agent: Option<String>,
        branch: Option<String>,
    ) -> Result<Response, RpcError> {
        let ticket = crate::tickets::get(&self.conn(), id)
            .map_err(failed)?
            .ok_or_else(|| RpcError::not_found(format!("no ticket with id {id}")))?;
        if ticket.state != ginka_protocol::model::TicketState::Open {
            return Err(RpcError::failed(crate::tickets::already(&ticket)));
        }
        let from = ticket
            .from_session
            .as_ref()
            .and_then(|from| session::get(&self.conn(), from).ok().flatten());
        let agent = agent
            .or_else(|| from.as_ref().map(|from| from.agent.clone()))
            .or_else(|| self.drivers.ids().first().map(|id| id.to_string()))
            .ok_or_else(|| RpcError::failed("no agent is available to start the ticket"))?;
        let workspace = match branch.filter(|branch| !branch.trim().is_empty()) {
            None => ticket.workspace.clone(),
            Some(branch) => {
                let project = self.worktree(&ticket.workspace)?.project;
                match self.handle(Request::CreateWorkspace {
                    project,
                    branch,
                    base: None,
                })? {
                    Response::Workspace { workspace } => workspace.worktree.workspace_id(),
                    other => {
                        return Err(RpcError::failed(format!("unexpected answer {other:?}")));
                    }
                }
            }
        };
        let access_mode = from
            .as_ref()
            .map(|from| from.access_mode)
            .unwrap_or_default();
        let Response::Session { session } = self.start_session(
            workspace,
            &agent,
            ticket.prompt.clone(),
            None,
            None,
            None,
            None,
            access_mode,
            None,
        )?
        else {
            return Err(RpcError::failed("the session did not start"));
        };
        crate::tickets::close(
            &self.conn(),
            id,
            ginka_protocol::model::TicketState::Started,
            Some(&session.id),
            now(),
        )
        .map_err(failed)?;
        self.events.emit(DaemonEvent::TicketsChanged {
            workspace: ticket.workspace,
        });
        Ok(Response::Session { session })
    }

    /// Send one session's words to another, saying who they are from and how
    /// to answer: an agent that receives an unsigned prompt takes it for the
    /// reader's and has no way to reply.
    fn message_session(
        &mut self,
        from: &SessionId,
        to: &SessionId,
        text: &str,
    ) -> Result<Response, RpcError> {
        if from == to {
            return Err(RpcError::failed("a session cannot message itself"));
        }
        let sender = self.session(from)?;
        self.session(to)?;
        let text = text.trim();
        if text.is_empty() {
            return Err(RpcError::failed("the message is empty"));
        }
        self.send_message(to, framed_message(&sender, text))
    }

    /// Send the front of an idle session's queue, the way a follow-up is sent.
    fn dispatch_front(&mut self, session: &SessionId) -> Result<(), RpcError> {
        if let Some(message) = self.sessions.take_front(session) {
            self.send_message(session, message.text)?;
        }
        Ok(())
    }

    /// Compact one provider thread as its own resumed turn.
    ///
    /// Refusing active sessions is load-bearing: a compact command must never
    /// be steered into, or queued behind, ordinary work where it could be
    /// interpreted as user text at the wrong boundary.
    fn compact_session(&mut self, id: &SessionId) -> Result<Response, RpcError> {
        let stored = self.session(id)?;
        if self.sessions.is_running(id) {
            return Err(RpcError::failed(
                "the session is still working; compact after the turn finishes",
            ));
        }
        if stored.vendor_session_id.is_none() {
            return Err(RpcError::failed(
                "the session has no provider thread to compact",
            ));
        }
        let vendor_session_id = stored.vendor_session_id.as_deref().expect("checked above");
        let worktree = self.worktree(&stored.workspace)?;
        let driver = self.driver(&stored.agent)?;
        let mut spec = SessionSpec::new(&worktree.path, "")
            .with_model(stored.model.clone())
            .with_reasoning_effort(stored.reasoning_effort.clone())
            .with_service_tier(stored.service_tier.clone())
            .with_access_mode(stored.access_mode)
            .with_mcp_servers(self.mcp_servers(&worktree.path, id));
        for (key, value) in crate::account::env_layer(
            &self.settings,
            &self.paths,
            &stored.account,
            driver.home_variable(),
        ) {
            spec = spec.with_env(key, value);
        }
        self.sessions
            .compact(id.clone(), driver, spec, vendor_session_id)
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

    /// Write a pull request's details on a cheap model and open it with them,
    /// off the request path; the outcome is pushed as `PullRequestOpened`.
    /// The agent and login are chosen as for a commit message.
    fn create_generated_pull_request(
        &mut self,
        workspace: WorkspaceId,
        agent: Option<&str>,
        draft: bool,
    ) -> Result<Response, RpcError> {
        let worktree = self.worktree(&workspace)?;
        let base = self.project(&worktree.project)?.default_branch;
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
        let project = worktree.project.clone();
        std::thread::spawn(move || {
            let outcome = crate::pr_details::generate(driver.as_ref(), &worktree.path, &env, &base)
                .and_then(|details| {
                    git::create_pull_request_with(
                        &worktree.path,
                        draft,
                        Some((&details.title, &details.body)),
                    )
                });
            let (url, error) = match outcome {
                Ok(url) => (Some(url), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            if url.is_some() {
                events.emit(DaemonEvent::WorkspacesChanged { project });
            }
            events.emit(DaemonEvent::PullRequestOpened {
                workspace,
                url,
                error,
            });
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
        let roots = crate::skills::default_roots(self.home().as_deref(), &projects);
        crate::skills::discover(&roots).map_err(failed)
    }

    /// Resolve a session, or say it is not there.
    /// Mark which added lines in `files` the workspace's recent turns wrote
    /// (`crate::attribution`). Only turns since the last commit count — an
    /// older turn's lines are in the commit, not the diff — and at most the
    /// newest [`ATTRIBUTION_TURNS`], so a long session does not make every
    /// refresh re-read its whole history. Failing to work it out leaves the
    /// lines unmarked rather than failing the diff.
    fn attribute(
        &mut self,
        workspace: &WorkspaceId,
        path: &std::path::Path,
        files: &mut [ginka_protocol::model::FileChange],
    ) {
        let since = git::last_commit_time(path).unwrap_or(i64::MIN);
        let conn = self.conn();
        let Ok(checkpoints) = checkpoint::list(&conn, workspace) else {
            return;
        };
        let turns: Vec<(String, String)> = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.has_turn_start && checkpoint.created_at >= since)
            .take(ATTRIBUTION_TURNS)
            .filter_map(|checkpoint| {
                checkpoint::turn_commits(&conn, &checkpoint.id)
                    .ok()
                    .flatten()
            })
            .collect();
        drop(conn);
        let read = |start: &str, end: &str| crate::attribution::turn_lines(path, start, end);
        match self.turn_lines.added_lines(&turns, read) {
            Ok(by_agent) => crate::attribution::mark(files, &by_agent),
            Err(error) => tracing::debug!(%error, "could not attribute the diff's lines"),
        }
    }

    /// Run `provider`'s own CLI with `args` (`mcp add …` / `mcp remove …`),
    /// with the binary Ginka is configured to use for it. Project and local
    /// scopes are a project's, so they run in `workspace`'s worktree and
    /// need one; user scope runs in the home directory. The vendor's own
    /// refusal is the answer when it refuses.
    fn run_vendor_mcp(
        &self,
        provider: &str,
        workspace: Option<&WorkspaceId>,
        scope: ginka_protocol::model::McpScope,
        args: &[String],
    ) -> Result<(), RpcError> {
        let driver = self.driver(provider)?;
        let dir = match (scope, workspace) {
            (ginka_protocol::model::McpScope::User, _) => {
                dirs::home_dir().ok_or_else(|| RpcError::failed("there is no home directory"))?
            }
            (_, Some(workspace)) => self.worktree(workspace)?.path,
            (_, None) => {
                return Err(RpcError::failed(
                    "a project or local MCP server needs the workspace whose project it is for",
                ));
            }
        };
        let mut command = std::process::Command::new(driver.program());
        crate::tool_path::apply(&mut command);
        let output = command
            .args(args)
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| {
                RpcError::failed(format!("could not run {}: {error}", driver.program()))
            })?;
        if output.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        let said = if said.is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            said.to_string()
        };
        Err(RpcError::failed(format!(
            "{} refused: {said}",
            driver.program()
        )))
    }

    /// Give `prompt` to the workspace's latest conversation — queued if it is
    /// working — or, when `agent` names another or there is none, to a new
    /// conversation on that agent. The conversation that did the work knows
    /// why it did it, which is most of fixing what went wrong with it.
    /// Refuse to take away somewhere an agent is working: a removal waits
    /// until every session running in a workspace `affected` picks has
    /// stopped, and says which.
    fn refuse_while_running(
        &self,
        affected: impl Fn(&WorkspaceId) -> bool,
    ) -> Result<(), RpcError> {
        let running: Vec<String> = session::list(&self.conn(), None)
            .map_err(failed)?
            .into_iter()
            .filter(|session| affected(&session.workspace) && self.sessions.is_running(&session.id))
            .map(|session| session.id.0)
            .collect();
        if running.is_empty() {
            return Ok(());
        }
        Err(RpcError::failed(format!(
            "an agent is still running there ({}); stop it first",
            running.join(", ")
        )))
    }

    fn hand_to_agent(
        &mut self,
        workspace: WorkspaceId,
        agent: Option<String>,
        prompt: String,
    ) -> Result<Response, RpcError> {
        let latest = session::latest_for_workspace(&self.conn(), &workspace).map_err(failed)?;
        match latest.filter(|latest| agent.as_ref().is_none_or(|agent| *agent == latest.agent)) {
            Some(latest) => {
                self.handle(Request::QueueMessage {
                    session: latest.id.clone(),
                    text: prompt,
                })?;
                Ok(Response::Session {
                    session: self.session(&latest.id)?,
                })
            }
            None => self.handle(Request::StartSession {
                workspace,
                agent: agent.unwrap_or_else(|| "claude".to_string()),
                prompt,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                account: None,
                access_mode: None,
                origin: None,
            }),
        }
    }

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

    /// Fire every scheduled job a tick at `now` owes, and say how many were
    /// due — skipped ones included. Prechecks run inline, holding the
    /// service; the daemon uses [`Service::take_due_cron`] and
    /// [`Service::fire_due_cron`] instead so it can let go of it meanwhile.
    pub fn run_due_cron(&mut self, now: chrono::DateTime<chrono::Local>) -> usize {
        let due = self.take_due_cron(now);
        for job in &due {
            let verdict = job.precheck();
            self.fire_due_cron(job, verdict);
        }
        due.len()
    }

    /// The jobs a tick at `now` owes a firing, each with where its precheck
    /// runs. Their clocks move on here, so a job is owed once however long
    /// its precheck takes.
    pub fn take_due_cron(&mut self, now: chrono::DateTime<chrono::Local>) -> Vec<DueCron> {
        let due = match crate::cron::take_due(&self.conn(), &now) {
            Ok(due) => due,
            Err(error) => {
                tracing::error!(%error, "could not read the scheduled jobs");
                return Vec::new();
            }
        };
        due.into_iter()
            .map(|job| {
                let precheck = job.precheck.clone().and_then(|command| {
                    let workspace = job
                        .workspace
                        .clone()
                        .or_else(|| self.project_checkout(&job.project))?;
                    let path = self.worktree(&workspace).ok()?.path;
                    Some((command, path))
                });
                DueCron { job, precheck }
            })
            .collect()
    }

    /// Fire one job [`Service::take_due_cron`] returned, unless its precheck
    /// said not to — in which case the skip is recorded with the reason.
    pub fn fire_due_cron(&mut self, due: &DueCron, verdict: crate::cron::Precheck) {
        match verdict {
            crate::cron::Precheck::Pass => self.fire_cron(&due.job),
            crate::cron::Precheck::Skip(reason) => {
                let _ = crate::cron::record_run(
                    &self.conn(),
                    due.job.id,
                    chrono::Utc::now().timestamp(),
                    ginka_protocol::model::CronOutcome::Skipped,
                    Some(&reason),
                );
            }
        }
    }

    /// Whether the rate table should be fetched again at `now`: never when
    /// the settings say not to, otherwise when there is none or it is a day
    /// old. The fetch itself is the daemon's, outside the service's lock.
    pub fn rates_due(&self, now: i64) -> Option<std::path::PathBuf> {
        if !self.settings.fetch_rates {
            return None;
        }
        let due = self
            .rates
            .as_ref()
            .is_none_or(|rates| rates.is_empty() || rates.is_stale(now));
        due.then(|| self.paths.rates_cache())
    }

    /// Use this rate table from now on.
    pub fn set_rates(&mut self, rates: Option<crate::usage::RateTable>) {
        if rates.is_some() {
            self.rates = rates;
        }
    }

    /// [`Service::run_due_cron`] on the daemon host's clock.
    pub fn run_due_cron_now(&mut self) -> usize {
        self.run_due_cron(chrono::Local::now())
    }

    /// Let go of every queue a usage limit held whose window has reset by
    /// `now`, and send the front of each whose session is idle. Answers how
    /// many were resumed.
    pub fn resume_limited_queues(&mut self, now: i64) -> usize {
        let due = self.sessions.resume_due_queues(now);
        for session in &due {
            if let Err(error) = self.dispatch_front(session) {
                tracing::warn!(error = %error.message, %session, "could not resume after the limit");
            }
        }
        due.len()
    }

    /// [`Service::take_due_cron`] on the daemon host's clock.
    pub fn take_due_cron_now(&mut self) -> Vec<DueCron> {
        self.take_due_cron(chrono::Local::now())
    }

    /// Fire one job: skip it while what it last started is still running,
    /// otherwise start its conversation or its terminal, and record the run.
    fn fire_cron(&mut self, job: &ginka_protocol::model::CronJob) {
        use ginka_protocol::model::{CronOutcome, CronVia};
        let now = chrono::Utc::now().timestamp();
        let record = |service: &Self, outcome: CronOutcome, detail: Option<&str>| {
            crate::cron::record_run(&service.conn(), job.id, now, outcome, detail)
        };
        // A reminder goes into its conversation — queued behind a turn that
        // is running, rather than skipped like an overlapping job: what the
        // reader asked to be reminded of still needs saying.
        if let Some(target) = &job.session {
            let sent = self.handle(Request::QueueMessage {
                session: target.clone(),
                text: job.body.clone(),
            });
            let _ = match sent {
                Ok(_) => record(self, CronOutcome::Started, Some(&target.0)),
                Err(error) => record(self, CronOutcome::Failed, Some(&error.message)),
            };
            return;
        }
        let workspace = match &job.workspace {
            Some(workspace) => Some(workspace.clone()),
            None => self.project_checkout(&job.project),
        };
        let Some(workspace) = workspace else {
            let _ = record(
                self,
                CronOutcome::Failed,
                Some(&format!("{} has no checkout to run in", job.project)),
            );
            return;
        };
        let last = crate::cron::last_started(&self.conn(), job.id).unwrap_or_default();
        let still_running = match job.via {
            CronVia::Chat => last
                .session
                .as_ref()
                .is_some_and(|session| self.sessions.is_running(&SessionId(session.clone()))),
            CronVia::Terminal => last.terminal.as_ref().is_some_and(|terminal| {
                self.terminals
                    .list(&workspace)
                    .iter()
                    .any(|open| &open.id.0 == terminal)
            }),
        };
        if still_running {
            let _ = record(
                self,
                CronOutcome::Skipped,
                Some("the previous run is still going"),
            );
            return;
        }

        match job.via {
            CronVia::Chat => {
                let started = self.handle(Request::StartSession {
                    workspace,
                    agent: job.agent.clone().unwrap_or_default(),
                    prompt: job.body.clone(),
                    model: None,
                    reasoning_effort: None,
                    service_tier: None,
                    account: None,
                    access_mode: None,
                    origin: None,
                });
                match started {
                    Ok(Response::Session { session }) => {
                        let _ = record(self, CronOutcome::Started, Some(&session.id.0));
                        let _ = crate::cron::set_last_started(
                            &self.conn(),
                            job.id,
                            &crate::cron::LastStarted {
                                session: Some(session.id.0),
                                terminal: None,
                            },
                        );
                    }
                    Ok(other) => {
                        let _ = record(self, CronOutcome::Failed, Some(&format!("{other:?}")));
                    }
                    Err(error) => {
                        let _ = record(self, CronOutcome::Failed, Some(&error.message));
                    }
                }
            }
            CronVia::Terminal => {
                let Ok(run) = record(self, CronOutcome::Started, None) else {
                    return;
                };
                let path = match self.worktree(&workspace) {
                    Ok(worktree) => worktree.path,
                    Err(error) => {
                        let _ = crate::cron::set_run(
                            &self.conn(),
                            run,
                            CronOutcome::Failed,
                            Some(&error.message),
                        );
                        return;
                    }
                };
                let conn = self.conn.clone();
                let opened = self.terminals.open_command(
                    &workspace,
                    &path,
                    crate::terminal::TerminalCommand {
                        program: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                        args: vec!["-lc".into(), job.body.clone()],
                        env: Vec::new(),
                        title: job.name.clone(),
                        on_exit: Some(Box::new(move || {
                            let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
                            let _ =
                                crate::cron::finish_run(&conn, run, chrono::Utc::now().timestamp());
                        })),
                    },
                    24,
                    100,
                );
                match opened {
                    Ok(terminal) => {
                        let _ = crate::cron::set_run(
                            &self.conn(),
                            run,
                            CronOutcome::Started,
                            Some(&terminal.0),
                        );
                        let _ = crate::cron::set_last_started(
                            &self.conn(),
                            job.id,
                            &crate::cron::LastStarted {
                                session: None,
                                terminal: Some(terminal.0),
                            },
                        );
                    }
                    Err(error) => {
                        let _ = crate::cron::set_run(
                            &self.conn(),
                            run,
                            CronOutcome::Failed,
                            Some(&format!("{error:#}")),
                        );
                    }
                }
            }
        }
    }

    /// The workspace that is the project's own checkout — where a job with no
    /// workspace runs.
    fn project_checkout(&self, name: &ProjectName) -> Option<WorkspaceId> {
        let project = self.project(name).ok()?;
        let worktrees = project::list_worktrees(&self.conn(), name).ok()?;
        worktrees
            .iter()
            .find(|worktree| worktree.path == project.path)
            .or(worktrees.first())
            .map(|worktree| worktree.workspace_id())
    }

    /// Resolve a workspace id to the worktree it names.
    fn worktree(&self, workspace: &WorkspaceId) -> Result<Worktree, RpcError> {
        project::find_worktree(&self.conn(), workspace)
            .map_err(failed)?
            .ok_or_else(|| RpcError::not_found(format!("no workspace named {workspace}")))
    }

    /// The workspace whose worktree holds `path` — the innermost one, since
    /// a scratch or nested worktree can sit inside another's directory.
    ///
    /// Compared both as given and canonicalized, because an agent's working
    /// directory may name the worktree through a symlink (`/tmp` on macOS).
    fn workspace_holding(&self, path: &std::path::Path) -> Result<WorkspaceId, RpcError> {
        let canonical = std::fs::canonicalize(path).ok();
        let conn = self.conn();
        let mut best: Option<(usize, WorkspaceId)> = None;
        for project in project::list_projects(&conn).map_err(failed)? {
            for worktree in project::list_worktrees(&conn, &project.name).map_err(failed)? {
                let root = std::fs::canonicalize(&worktree.path).unwrap_or(worktree.path.clone());
                let inside = path.starts_with(&worktree.path)
                    || canonical
                        .as_ref()
                        .is_some_and(|path| path.starts_with(&root));
                let depth = root.components().count();
                if inside && best.as_ref().is_none_or(|(deepest, _)| depth > *deepest) {
                    best = Some((depth, worktree.workspace_id()));
                }
            }
        }
        best.map(|(_, workspace)| workspace)
            .ok_or_else(|| RpcError::not_found(format!("no workspace holds {}", path.display())))
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
        let queued = session
            .as_ref()
            .map(|session| self.sessions.queued_count(&session.id))
            .unwrap_or(0);
        let id = worktree.workspace_id();
        WorkspaceSummary {
            indexed: crate::tools::is_indexed(&worktree.path),
            worktree,
            status,
            session,
            last_commit_at,
            queued,
            pull_request: self.pull_requests.get(&id).cloned(),
            status_note: project::status_note(&self.conn(), &id).unwrap_or_default(),
        }
    }
}

/// How many recent turns line attribution reads at most.
const ATTRIBUTION_TURNS: usize = 20;

/// A scheduled job a tick owes a firing, and the probe to run before it.
#[derive(Debug, Clone)]
pub struct DueCron {
    /// The job, as stored when the tick took it.
    pub job: ginka_protocol::model::CronJob,
    /// Its precheck command and the checkout it runs in; `None` when the job
    /// has none, or has nowhere to run — which firing then reports.
    pub precheck: Option<(String, std::path::PathBuf)>,
}

impl DueCron {
    /// Run the precheck, if there is one. Blocks for up to
    /// [`crate::cron::PRECHECK_TIMEOUT`]; call it without the service held.
    pub fn precheck(&self) -> crate::cron::Precheck {
        match &self.precheck {
            Some((command, dir)) => {
                crate::cron::run_precheck(command, dir, crate::cron::PRECHECK_TIMEOUT)
            }
            None => crate::cron::Precheck::Pass,
        }
    }
}

/// One project's worth of work for the pull request poller.
#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestPlan {
    /// Where `gh` runs.
    pub root: std::path::PathBuf,
    /// Every workspace in the project and the branch it is on now.
    pub branches: Vec<(WorkspaceId, String)>,
}

/// What another session's message looks like to the one receiving it: who
/// sent it, and the tool that answers it, above the words themselves.
pub fn framed_message(sender: &Session, text: &str) -> String {
    let title = sender.title.as_deref().unwrap_or("untitled");
    format!(
        "[Message from Ginka session {id} ({agent}, \"{title}\") in workspace {workspace}. \
         To reply, call ginka_session_send with session \"{id}\".]\n\n{text}",
        id = sender.id,
        agent = sender.agent,
        workspace = sender.workspace,
    )
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

    /// The projects whose pull requests the poller should read: each one's
    /// root and the branch every workspace in it is on.
    ///
    /// A plain folder, or a repository known to have no remote, has nothing
    /// to ask GitHub about.
    pub fn pull_request_plan(&self) -> Vec<PullRequestPlan> {
        let Ok(projects) = self.projects() else {
            return Vec::new();
        };
        projects
            .into_iter()
            .filter(|project| project.kind == ginka_protocol::ProjectKind::Git)
            .filter(|project| project.has_origin != Some(false))
            .filter_map(|project| {
                let worktrees = project::list_worktrees(&self.conn(), &project.name).ok()?;
                let branches = worktrees
                    .iter()
                    .map(|worktree| (worktree.workspace_id(), worktree.branch.clone()))
                    .collect();
                Some(PullRequestPlan {
                    root: project.path,
                    branches,
                })
            })
            .collect()
    }

    /// Keep what a read of one project's pull requests found for each of its
    /// workspaces, and push what changed.
    pub fn set_pull_requests(
        &mut self,
        found: Vec<(WorkspaceId, Option<ginka_protocol::model::PullRequest>)>,
    ) {
        for (workspace, pull_request) in found {
            self.note_pull_request(workspace, pull_request);
        }
    }

    /// Remember one workspace's pull request, telling clients only when it is
    /// news.
    fn note_pull_request(
        &mut self,
        workspace: WorkspaceId,
        pull_request: Option<ginka_protocol::model::PullRequest>,
    ) {
        if self.pull_requests.get(&workspace) == pull_request.as_ref() {
            return;
        }
        match &pull_request {
            Some(known) => self.pull_requests.insert(workspace.clone(), known.clone()),
            None => self.pull_requests.remove(&workspace),
        };
        self.events.emit(DaemonEvent::WorkspacePullRequestChanged {
            workspace,
            pull_request,
        });
    }

    /// Do what the project asked for in a new worktree.
    ///
    /// A fresh checkout has none of the files the repository deliberately does
    /// not track, and an agent started there fails on its first command for a
    /// reason that has nothing to do with its task. What went wrong is logged
    /// rather than returned: the worktree exists by now, and removing it
    /// because an install failed would throw away the branch the user asked
    /// for.
    fn set_up(
        &self,
        project: &std::path::Path,
        worktree: &std::path::Path,
        workspace: &WorkspaceId,
    ) {
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
        // The files first, before anything can start in the worktree.
        let copying = crate::setup::Setup {
            copy: setup.copy.clone(),
            commands: Vec::new(),
        };
        let report = crate::setup::run(project, worktree, &copying);
        for problem in &report.problems {
            tracing::warn!(problem, worktree = %worktree.display(), "setting up the worktree");
        }
        // The commands in a terminal of the workspace's own, as Orca runs
        // its setup script: an install is minutes, and the reader should see
        // it happen rather than wait on a request that says nothing.
        if !setup.commands.is_empty() {
            let command = crate::terminal::TerminalCommand {
                program: "sh".to_string(),
                args: crate::setup::terminal_args(&setup.commands),
                env: Vec::new(),
                title: "setup".to_string(),
                on_exit: None,
            };
            if let Err(error) = self
                .terminals
                .open_command(workspace, worktree, command, 24, 100)
            {
                tracing::warn!(%error, "could not start the project's setup");
            }
        }
        tracing::info!(copied = report.copied.len(), "set up the worktree");
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

fn goal_preamble(objective: &str) -> String {
    format!(
        "The conversation has a durable goal: {objective}\nWork toward this goal on this turn. Report concrete progress and remaining work. The goal stays active until the user sends /goal done."
    )
}

fn failed(error: impl std::fmt::Display) -> RpcError {
    RpcError::failed(error.to_string())
}

/// Replace every value in an `env` map with `[set]`: which variables an agent
/// is given is the reader's to see; what they hold — keys, tokens — is not
/// for the wire (`docs/accounts.md` §10).
fn redact_env(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                match field {
                    serde_json::Value::Object(env) if key == "env" => {
                        for secret in env.values_mut() {
                            *secret = serde_json::Value::String("[set]".into());
                        }
                    }
                    other => redact_env(other),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_env),
        _ => {}
    }
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
