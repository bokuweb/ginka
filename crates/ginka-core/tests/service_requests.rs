//! The daemon's request surface, exercised without a socket.
//!
//! `Service` is the whole of what the daemon does; the transport around it
//! only frames JSON. Testing it here means the capability list is pinned
//! without spawning a server, and it is the same code path the CLI reaches
//! over the wire.

mod support;

use ginka_core::service::{EventSink, Service};
use ginka_core::{Paths, db};
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::model::ChangeSource;
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{ProjectName, WorkspaceId};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Collects everything the service announced, so a test can assert that a
/// mutation was pushed to other clients and not only written to the database.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<DaemonEvent>>,
}

impl Recorder {
    fn taken(&self) -> Vec<DaemonEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

impl EventSink for Recorder {
    fn emit(&self, event: DaemonEvent) {
        self.events.lock().unwrap().push(event);
    }
}

struct Fixture {
    service: Service,
    recorder: Arc<Recorder>,
    /// Kept alive: dropping it deletes the state directory.
    _home: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::configured(|service| service)
    }

    /// A fixture whose service the test has set up further.
    fn configured(configure: impl FnOnce(Service) -> Service) -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(home.path().join("state"));
        paths.ensure().unwrap();
        let recorder = Arc::new(Recorder::default());
        let service = configure(Service::new(
            paths,
            db::open_in_memory().unwrap(),
            recorder.clone(),
        ));
        Self {
            service,
            recorder,
            _home: home,
            work: tempfile::tempdir().unwrap(),
        }
    }

    fn ask(&mut self, request: Request) -> Response {
        self.service
            .handle(request)
            .unwrap_or_else(|error| panic!("request failed: {error}"))
    }

    /// Register a repository with one commit and return its project name.
    fn with_project(&mut self) -> ProjectName {
        let root = self.work.path().join("comet");
        support::repository(&root);
        match self.ask(Request::AddProject {
            path: root,
            label: None,
        }) {
            Response::Project { project } => project.name,
            other => panic!("expected a project, got {other:?}"),
        }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.work.path().join("comet")
    }
}

#[test]
fn ping_answers_ack() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.ask(Request::Ping), Response::Ack);
}

#[test]
fn adding_a_project_registers_it_and_announces_the_change() {
    let mut fixture = Fixture::new();
    let name = fixture.with_project();
    assert_eq!(name.0, "comet");

    let events = fixture.recorder.taken();
    assert!(
        events.contains(&DaemonEvent::ProjectsChanged),
        "other clients have to learn about a new project: {events:?}"
    );
    assert!(
        events.contains(&DaemonEvent::WorkspacesChanged {
            project: name.clone()
        }),
        "registering adopts the repository's worktrees: {events:?}"
    );

    match fixture.ask(Request::ListProjects) {
        Response::Projects { projects } => assert_eq!(projects.len(), 1),
        other => panic!("expected projects, got {other:?}"),
    }
}

#[test]
fn workspace_summaries_report_the_daemon_hosts_semantic_index_state() {
    let mut fixture = Fixture::new();
    fixture.with_project();

    let listed = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(!listed[0].indexed);

    std::fs::create_dir(fixture.repo().join(ginka_core::tools::ZVEC_GREP_INDEX_DIR)).unwrap();
    let listed = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(listed[0].indexed);
}

#[test]
fn adding_a_project_keeps_the_display_name_across_the_service_boundary() {
    let mut fixture = Fixture::new();
    let root = fixture.work.path().join("client-checkout");
    support::repository(&root);

    let project = match fixture.ask(Request::AddProject {
        path: root,
        label: Some("Comet".into()),
    }) {
        Response::Project { project } => project,
        other => panic!("expected a project, got {other:?}"),
    };

    assert_eq!(project.name.0, "client-checkout");
    assert_eq!(project.label.as_deref(), Some("Comet"));
}

#[test]
fn adding_a_project_twice_updates_it_rather_than_failing() {
    // Re-registering is how a user tells Ginka the repository moved.
    let mut fixture = Fixture::new();
    fixture.with_project();
    fixture.with_project();
    match fixture.ask(Request::ListProjects) {
        Response::Projects { projects } => assert_eq!(projects.len(), 1),
        other => panic!("expected projects, got {other:?}"),
    }
}

#[test]
fn creating_a_workspace_puts_the_worktree_under_ginkas_own_directory() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    // Canonicalised on both sides: on macOS the state directory is reached
    // through /var, and git reports the /private/var it resolves to.
    let root = fixture
        .service
        .paths()
        .worktrees()
        .canonicalize()
        .expect("the worktree root exists");

    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "bright-harbor".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };

    assert_eq!(workspace.worktree.branch, "bright-harbor");
    assert_eq!(workspace.id(), WorkspaceId::new(&project, "bright-harbor"));
    assert!(
        workspace
            .worktree
            .path
            .canonicalize()
            .unwrap()
            .starts_with(&root),
        "{} must live under {}",
        workspace.worktree.path.display(),
        root.display()
    );
    assert!(workspace.worktree.path.join("README.md").is_file());
    assert!(
        fixture
            .recorder
            .taken()
            .contains(&DaemonEvent::WorkspacesChanged { project })
    );
}

#[test]
fn listing_workspaces_reconciles_against_git_first() {
    // A worktree added with the user's own git must show up without the app
    // having been told about it.
    let mut fixture = Fixture::new();
    fixture.with_project();
    let outside = fixture.work.path().join("outside");
    support::git(
        &fixture.repo(),
        &[
            "worktree",
            "add",
            "-b",
            "outside",
            outside.to_str().unwrap(),
            "main",
        ],
    );

    match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => {
            let branches: Vec<&str> = workspaces
                .iter()
                .map(|w| w.worktree.branch.as_str())
                .collect();
            assert!(branches.contains(&"outside"), "{branches:?}");
            assert!(branches.contains(&"main"), "{branches:?}");
        }
        other => panic!("expected workspaces, got {other:?}"),
    }
}

#[test]
fn a_workspace_summary_carries_the_worktrees_git_status() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "dirty-work".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(workspace.worktree.path.join("new.txt"), "scratch").unwrap();

    let summaries = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    let found = summaries
        .iter()
        .find(|summary| summary.id() == workspace.id())
        .expect("the workspace is listed");
    assert!(found.status.dirty, "an untracked file makes it dirty");
    assert!(found.last_commit_at.is_some(), "it has a commit");
}

#[test]
fn a_file_save_crosses_the_service_and_refuses_a_stale_editor() {
    let mut fixture = Fixture::new();
    fixture.with_project();
    let workspace = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces[0].id(),
        other => panic!("expected workspaces, got {other:?}"),
    };
    let opened = match fixture.ask(Request::ReadFile {
        workspace: workspace.clone(),
        path: "README.md".into(),
    }) {
        Response::FileContent { file } => file,
        other => panic!("expected file content, got {other:?}"),
    };
    let saved = match fixture.ask(Request::WriteFile {
        workspace: workspace.clone(),
        path: opened.path.clone(),
        text: "edited in Ginka\n".into(),
        expected_revision: opened.revision.clone(),
    }) {
        Response::FileContent { file } => file,
        other => panic!("expected file content, got {other:?}"),
    };
    assert_eq!(saved.text, "edited in Ginka\n");

    let stale = fixture.service.handle(Request::WriteFile {
        workspace,
        path: opened.path,
        text: "overwrite\n".into(),
        expected_revision: opened.revision,
    });
    assert!(stale.is_err(), "the first save changed the revision");
}

#[test]
fn project_search_tags_active_workspaces_and_shares_one_limit() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    std::fs::write(fixture.repo().join("README.md"), "needle on main\n").unwrap();
    std::fs::write(fixture.repo().join("needle-main.txt"), "path hit\n").unwrap();

    let second = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "second".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(second.worktree.path.join("README.md"), "needle on second\n").unwrap();
    std::fs::write(second.worktree.path.join("needle-second.txt"), "path hit\n").unwrap();

    let archived = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "archived".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(
        archived.worktree.path.join("README.md"),
        "needle in archive\n",
    )
    .unwrap();
    fixture.ask(Request::ArchiveWorkspace {
        workspace: archived.id(),
        archived: true,
    });

    let (files, matches) = match fixture.ask(Request::SearchProject {
        project: project.clone(),
        query: "needle".into(),
        limit: Some(2),
    }) {
        Response::WorkspaceMatches { files, matches } => (files, matches),
        other => panic!("expected workspace matches, got {other:?}"),
    };
    assert_eq!(matches.len(), 2, "the limit is shared across worktrees");
    assert_eq!(
        files.len(),
        2,
        "path matches have their own shared project limit"
    );
    assert!(files.iter().any(|hit| hit.path == "needle-main.txt"));
    assert!(files.iter().any(|hit| hit.path == "needle-second.txt"));
    assert!(files.iter().all(|hit| hit.workspace != archived.id()));
    assert!(matches.iter().all(|hit| hit.workspace != archived.id()));
    assert!(
        matches
            .iter()
            .any(|hit| hit.workspace == WorkspaceId::new(&project, "main"))
    );
    assert!(matches.iter().any(|hit| hit.workspace == second.id()));
}

#[test]
fn removing_a_dirty_workspace_needs_force() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "throwaway".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(workspace.worktree.path.join("new.txt"), "unsaved work").unwrap();

    let refused = fixture.service.handle(Request::RemoveWorkspace {
        workspace: workspace.id(),
        force: false,
    });
    assert!(refused.is_err(), "unsaved work must not vanish silently");

    fixture.ask(Request::RemoveWorkspace {
        workspace: workspace.id(),
        force: true,
    });
    assert!(!workspace.worktree.path.exists());
}

#[test]
fn pinning_survives_the_next_reconciliation() {
    // Pinning is ours, not git's, so a sync that re-reads git must not drop it.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "keep-me".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };

    fixture.ask(Request::PinWorkspace {
        workspace: workspace.id(),
        pinned: true,
    });
    let listed = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(
        listed
            .iter()
            .find(|summary| summary.id() == workspace.id())
            .expect("still listed")
            .worktree
            .pinned
    );
}

#[test]
fn archiving_a_workspace_survives_reconciliation_and_can_be_undone() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "later".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };

    fixture.ask(Request::ArchiveWorkspace {
        workspace: workspace.id(),
        archived: true,
    });
    let listed = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(
        listed
            .iter()
            .find(|summary| summary.id() == workspace.id())
            .expect("archiving keeps the workspace registered")
            .worktree
            .archived
    );

    // Listing reconciles against git. Archive state belongs to Ginka and must
    // not be overwritten by that reconciliation.
    let reconciled = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(
        reconciled
            .iter()
            .find(|summary| summary.id() == workspace.id())
            .expect("reconciliation keeps the archived workspace")
            .worktree
            .archived
    );

    fixture.ask(Request::ArchiveWorkspace {
        workspace: workspace.id(),
        archived: false,
    });
    let restored = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(
        !restored
            .iter()
            .find(|summary| summary.id() == workspace.id())
            .expect("restoring keeps the workspace")
            .worktree
            .archived
    );
}

#[test]
fn removing_a_project_forgets_it_without_touching_the_users_code() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    fixture.ask(Request::RemoveProject {
        project: project.clone(),
    });

    match fixture.ask(Request::ListProjects) {
        Response::Projects { projects } => assert!(projects.is_empty()),
        other => panic!("expected projects, got {other:?}"),
    }
    assert!(
        fixture.repo().join("README.md").is_file(),
        "the daemon registered the repository, it did not create it"
    );
}

#[test]
fn naming_a_project_that_does_not_exist_is_a_not_found_error() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::ListWorkspaces {
            project: Some(ProjectName("absent".into())),
        })
        .expect_err("there is no such project");
    assert_eq!(error.code, "not_found");
    assert!(error.message.contains("absent"), "{}", error.message);
}

#[test]
fn naming_a_workspace_that_does_not_exist_is_a_not_found_error() {
    let mut fixture = Fixture::new();
    fixture.with_project();
    let error = fixture
        .service
        .handle(Request::PinWorkspace {
            workspace: WorkspaceId("comet/absent".into()),
            pinned: true,
        })
        .expect_err("there is no such workspace");
    assert_eq!(error.code, "not_found");
}

#[test]
fn a_plain_folder_has_one_workspace_an_agent_can_run_in() {
    // Plenty of useful agent work happens outside a repository. The folder is
    // the workspace; there is nothing to branch.
    let mut fixture = Fixture::new();
    let notes = fixture.work.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let project = match fixture.ask(Request::AddProject {
        path: notes.clone(),
        label: None,
    }) {
        Response::Project { project } => project,
        other => panic!("expected a project, got {other:?}"),
    };

    let workspaces = match fixture.ask(Request::ListWorkspaces {
        project: Some(project.name.clone()),
    }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert_eq!(workspaces.len(), 1);
    assert_eq!(
        workspaces[0].worktree.path.canonicalize().unwrap(),
        notes.canonicalize().unwrap()
    );
    // And it is addressable, which is what an agent needs.
    fixture.ask(Request::PinWorkspace {
        workspace: workspaces[0].id(),
        pinned: true,
    });
}

#[test]
fn a_scratch_workspace_needs_no_project_at_all() {
    // waku's "just start an agent" flow: somewhere to work, made on the spot.
    let mut fixture = Fixture::new();
    let workspace = match fixture.ask(Request::CreateScratchWorkspace {
        name: Some("Try the parser".into()),
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };

    assert!(workspace.worktree.path.is_dir(), "it exists on disk");
    assert!(
        workspace.worktree.path.canonicalize().unwrap().starts_with(
            fixture
                .service
                .paths()
                .scratch_projects()
                .canonicalize()
                .unwrap()
        ),
        "scratch work lives under Ginka's own directory: {}",
        workspace.worktree.path.display()
    );
    // Dated, so a week of scratch work is still findable.
    let dated = workspace
        .worktree
        .path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(dated.len(), 10, "a YYYY-MM-DD directory, got {dated}");
    assert!(dated.starts_with("20"), "{dated}");
    assert!(workspace.worktree.path.ends_with("try-the-parser"));

    // It shows up like any other workspace.
    match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => {
            assert!(
                workspaces
                    .iter()
                    .any(|listed| listed.id() == workspace.id())
            )
        }
        other => panic!("expected workspaces, got {other:?}"),
    }
}

#[test]
fn a_second_scratch_of_the_same_name_gets_its_own_directory() {
    // Names repeat -- "fix", "test", "try again" -- and the second must not
    // land an agent in the first one's files.
    let mut fixture = Fixture::new();
    let first = match fixture.ask(Request::CreateScratchWorkspace {
        name: Some("fix".into()),
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let second = match fixture.ask(Request::CreateScratchWorkspace {
        name: Some("fix".into()),
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    assert_ne!(first.worktree.path, second.worktree.path);
    assert_ne!(first.id(), second.id());
}

#[test]
fn a_scratch_workspace_with_no_name_is_still_named() {
    let mut fixture = Fixture::new();
    let workspace = match fixture.ask(Request::CreateScratchWorkspace { name: None }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    assert!(!workspace.worktree.name.is_empty());
    assert!(workspace.worktree.path.is_dir());
}

/// A driver that records how often it was asked about itself.
struct CountingDriver {
    probes: Arc<std::sync::atomic::AtomicUsize>,
}

impl ginka_core::driver::AgentDriver for CountingDriver {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn display_name(&self) -> &'static str {
        "Counting"
    }
    fn models(&self) -> Vec<ginka_core::driver::ProviderModel> {
        Vec::new()
    }
    fn program(&self) -> &str {
        "/nonexistent/ginka-counting-agent"
    }
    fn probe_command(&self) -> ginka_core::driver::CommandSpec {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ginka_core::driver::CommandSpec::new(self.program())
    }
    fn parse_version(&self, _output: &str) -> Option<String> {
        None
    }
    fn start_command(
        &self,
        _spec: &ginka_core::driver::SessionSpec,
    ) -> ginka_core::driver::CommandSpec {
        ginka_core::driver::CommandSpec::new(self.program())
    }
    fn resume_command(
        &self,
        _spec: &ginka_core::driver::SessionSpec,
        _vendor: &str,
    ) -> ginka_core::driver::CommandSpec {
        ginka_core::driver::CommandSpec::new(self.program())
    }
    fn parse_line(
        &self,
        _line: &str,
        _state: &mut ginka_core::driver::ParseState,
    ) -> Vec<ginka_protocol::AgentEvent> {
        Vec::new()
    }
}

#[test]
fn an_agent_that_is_missing_is_reported_before_a_prompt_is_sent() {
    // Learning that an agent is not installed from a session that failed is
    // learning it too late.
    let mut fixture = Fixture::new();
    match fixture.ask(Request::ListAgents) {
        Response::Agents { agents } => {
            let claude = agents
                .iter()
                .find(|agent| agent.id == "claude")
                .expect("the build ships a claude driver");
            assert_eq!(claude.display_name, "Claude Code");
            assert!(!claude.models.is_empty());
        }
        other => panic!("expected agents, got {other:?}"),
    }
}

#[test]
fn the_agent_probe_is_not_re_run_on_every_ask() {
    // Probing shells out twice per agent and the sidebar asks on every tick;
    // an installed CLI does not come and go between them.
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(home.path().join("state"));
    paths.ensure().unwrap();
    let mut drivers = ginka_core::driver::Registry::empty();
    drivers.insert(Arc::new(CountingDriver {
        probes: probes.clone(),
    }));
    let mut service = Service::new(
        paths,
        db::open_in_memory().unwrap(),
        Arc::new(Recorder::default()),
    )
    .with_drivers(drivers);

    for _ in 0..5 {
        service.handle(Request::ListAgents).unwrap();
    }
    assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn the_service_reports_where_its_state_lives() {
    let fixture = Fixture::new();
    assert!(fixture.service.paths().root().ends_with("state"));
    assert!(Path::new(fixture.service.paths().root()).is_dir());
}

#[test]
fn a_file_can_be_staged_reverted_and_committed_on_its_own() {
    // The half of the review loop that is not a comment: some of what the
    // agent did is right, and the rest is to be thrown away.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "review".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let id = workspace.id();
    let worktree = &workspace.worktree.path;
    std::fs::write(worktree.join("keep.txt"), "worth keeping\n").unwrap();
    std::fs::write(worktree.join("wrong.txt"), "not worth keeping\n").unwrap();

    fixture.ask(Request::StageFile {
        workspace: id.clone(),
        path: "keep.txt".into(),
        staged: true,
    });
    let staged = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Staged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    let paths: Vec<&str> = staged.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["keep.txt"], "only the file that was staged");

    fixture.ask(Request::RevertFile {
        workspace: id.clone(),
        path: "wrong.txt".into(),
    });
    assert!(
        !worktree.join("wrong.txt").exists(),
        "a file the agent invented and the reader rejected is gone"
    );

    match fixture.ask(Request::Commit {
        workspace: id.clone(),
        message: "keep the good half".into(),
        all: false,
        amend: false,
    }) {
        Response::Committed { commit } => assert!(!commit.is_empty()),
        other => panic!("expected a commit, got {other:?}"),
    }
    let left = match fixture.ask(Request::WorkspaceChanges {
        workspace: id,
        source: ChangeSource::Uncommitted,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert!(left.is_empty(), "nothing is left over: {left:?}");
}

#[test]
fn one_hunk_can_be_staged_unstaged_and_discarded_across_the_service_boundary() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let baseline = (1..=30)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    std::fs::write(fixture.repo().join("README.md"), &baseline).unwrap();
    support::git(&fixture.repo(), &["add", "README.md"]);
    support::git(&fixture.repo(), &["commit", "-m", "long fixture"]);
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "partial-review".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let id = workspace.id();
    let worktree = workspace.worktree.path.clone();
    let edited = baseline
        .replace("line 2\n", "line two\n")
        .replace("line 29\n", "line twenty-nine\n");
    std::fs::write(worktree.join("README.md"), edited).unwrap();

    let unstaged = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Unstaged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(unstaged.files[0].hunks.len(), 2);
    let first_header = unstaged.files[0].hunks[0].header.clone();

    assert_eq!(
        fixture.ask(Request::StageHunk {
            workspace: id.clone(),
            path: "README.md".into(),
            header: first_header.clone(),
            staged: true,
        }),
        Response::Ack
    );
    let staged = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Staged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    let left = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Unstaged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(staged.files[0].hunks.len(), 1);
    assert!(
        staged.files[0].hunks[0]
            .lines
            .iter()
            .any(|line| line.text == "line two")
    );
    assert_eq!(left.files[0].hunks.len(), 1);
    assert!(
        left.files[0].hunks[0]
            .lines
            .iter()
            .any(|line| line.text == "line twenty-nine")
    );

    fixture.ask(Request::StageHunk {
        workspace: id.clone(),
        path: "README.md".into(),
        header: first_header.clone(),
        staged: false,
    });
    let staged = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Staged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    let unstaged = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Unstaged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert!(staged.is_empty());
    assert_eq!(unstaged.files[0].hunks.len(), 2);

    fixture.ask(Request::RevertHunk {
        workspace: id.clone(),
        path: "README.md".into(),
        header: first_header,
    });
    let left = match fixture.ask(Request::WorkspaceChanges {
        workspace: id,
        source: ChangeSource::Unstaged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(left.files[0].hunks.len(), 1);
    assert!(
        left.files[0].hunks[0]
            .lines
            .iter()
            .any(|line| line.text == "line twenty-nine")
    );
    assert!(
        !std::fs::read_to_string(worktree.join("README.md"))
            .unwrap()
            .contains("line two")
    );
}

#[test]
fn workspace_history_crosses_the_service_with_its_bound() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "history-reader".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let worktree = &workspace.worktree.path;
    std::fs::write(worktree.join("second.txt"), "second\n").unwrap();
    support::git(worktree, &["add", "second.txt"]);
    support::git(worktree, &["commit", "-m", "second"]);

    let commits = match fixture.ask(Request::WorkspaceHistory {
        workspace: workspace.id(),
        limit: Some(1),
    }) {
        Response::History { commits } => commits,
        other => panic!("expected history, got {other:?}"),
    };

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].summary, "second");
    assert_eq!(commits[0].parents.len(), 1);
}

#[test]
fn a_branch_can_be_listed_and_checked_out_without_re_keying_the_workspace() {
    // Rule 4: the id derives from the immutable name, so an agent — or a
    // person — switching branches inside a worktree changes the branch
    // column and nothing else.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "harbor".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let id = workspace.id();

    let branches = match fixture.ask(Request::ListBranches {
        workspace: id.clone(),
    }) {
        Response::Branches { branches } => branches,
        other => panic!("expected branches, got {other:?}"),
    };
    let here = branches
        .iter()
        .find(|branch| branch.current)
        .expect("one is current");
    assert_eq!(here.name, "harbor");
    let elsewhere = branches
        .iter()
        .find(|branch| !branch.current && branch.checked_out_at.is_some())
        .expect("the project's own checkout holds its branch");

    fixture.ask(Request::CheckoutBranch {
        workspace: id.clone(),
        branch: "harbor-v2".into(),
        create: true,
    });
    let listed = match fixture.ask(Request::ListWorkspaces {
        project: Some(project),
    }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    let same = listed
        .iter()
        .find(|summary| summary.id() == id)
        .expect("the id did not move with the branch");
    assert_eq!(same.worktree.branch, "harbor-v2");
    assert!(
        fixture
            .recorder
            .taken()
            .iter()
            .any(|event| matches!(event, DaemonEvent::WorkspacesChanged { .. })),
        "other windows are told"
    );

    // A branch another worktree holds cannot be taken: git says so, and the
    // refusal names it rather than leaving a half-switched tree.
    let error = fixture
        .service
        .handle(Request::CheckoutBranch {
            workspace: id,
            branch: elsewhere.name.clone(),
            create: false,
        })
        .unwrap_err();
    assert!(error.message.contains(&elsewhere.name), "{}", error.message);
}

#[test]
fn a_projects_skills_are_listed_and_switched_off_without_being_deleted() {
    // N11: the agents' own skills, managed from here. A project's copy is
    // found under its checkout; disabling renames the file every tool looks
    // for, so all of them stop seeing it, and nothing is lost.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let skill = fixture.repo().join(".claude/skills/release-notes");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Write the notes for a release\n---\n# Notes\n",
    )
    .unwrap();

    let listed = match fixture.ask(Request::ListSkills {
        project: Some(project.clone()),
    }) {
        Response::Skills { skills, truncated } => {
            assert!(!truncated);
            skills
        }
        other => panic!("expected skills, got {other:?}"),
    };
    let found = listed
        .iter()
        .find(|skill| skill.name == "release-notes")
        .expect("the project's skill is in the library");
    assert!(found.enabled);
    assert_eq!(
        found.description.as_deref(),
        Some("Write the notes for a release")
    );
    let install = found
        .installs
        .iter()
        .find(|install| install.scope == ginka_protocol::model::SkillScope::Project)
        .expect("installed under the project");
    assert_eq!(install.root_label, project.0);
    // The project's path was canonicalized when it was registered, which on
    // macOS resolves `/var` to `/private/var`.
    assert_eq!(install.directory, skill.canonicalize().unwrap());

    fixture.ask(Request::SetSkillEnabled {
        name: "release-notes".into(),
        enabled: false,
        project: Some(project.clone()),
    });
    assert!(!skill.join("SKILL.md").exists());
    assert!(
        skill.join("SKILL.md.disabled").is_file(),
        "renamed, not removed"
    );
    match fixture.ask(Request::ListSkills {
        project: Some(project.clone()),
    }) {
        Response::Skills { skills, .. } => {
            let found = skills
                .iter()
                .find(|skill| skill.name == "release-notes")
                .unwrap();
            assert!(!found.enabled, "and the library says so");
        }
        other => panic!("expected skills, got {other:?}"),
    }

    fixture.ask(Request::SetSkillEnabled {
        name: "release-notes".into(),
        enabled: true,
        project: Some(project),
    });
    assert!(skill.join("SKILL.md").is_file());

    let error = fixture
        .service
        .handle(Request::SetSkillEnabled {
            name: "no-such-skill".into(),
            enabled: false,
            project: None,
        })
        .unwrap_err();
    assert!(error.message.contains("no-such-skill"));
}

#[test]
fn a_new_worktree_gets_what_the_project_said_it_needs() {
    // A fresh checkout has none of the files the repository deliberately does
    // not track, and an agent started there fails on its first command for a
    // reason that has nothing to do with its task.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let root = fixture.repo();
    std::fs::create_dir_all(root.join(".ginka")).unwrap();
    std::fs::write(
        root.join(".ginka/config.json"),
        r#"{"copy": [".env"], "commands": ["echo ready > .setup-ran"]}"#,
    )
    .unwrap();
    std::fs::write(root.join(".env"), "TOKEN=secret\n").unwrap();

    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "with-setup".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };

    let worktree = &workspace.worktree.path;
    assert_eq!(
        std::fs::read_to_string(worktree.join(".env")).unwrap(),
        "TOKEN=secret\n",
        "the untracked file the project named came across"
    );
    // The commands run in a terminal of the workspace's own, where the
    // reader can watch them, rather than inside the request.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !worktree.join(".setup-ran").is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "the setup command never ran in the new worktree"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    match fixture.ask(Request::WorkspaceTerminals {
        workspace: workspace.id(),
    }) {
        Response::Terminals { terminals } => assert!(
            terminals.iter().any(|terminal| terminal.title == "setup"),
            "in a terminal called setup: {terminals:?}"
        ),
        other => panic!("expected terminals, got {other:?}"),
    }
}

#[test]
fn the_poller_pushes_a_status_that_changed_and_stays_quiet_otherwise() {
    // A push per workspace per minute is a push clients learn to ignore.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "polled".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    fixture.recorder.taken();

    fixture.service.poll_statuses();
    let first: Vec<_> = fixture
        .recorder
        .taken()
        .into_iter()
        .filter(|event| matches!(event, DaemonEvent::WorkspaceStatusChanged { .. }))
        .collect();
    assert!(!first.is_empty(), "the first look at a workspace is news");

    fixture.service.poll_statuses();
    let again: Vec<_> = fixture
        .recorder
        .taken()
        .into_iter()
        .filter(|event| matches!(event, DaemonEvent::WorkspaceStatusChanged { .. }))
        .collect();
    assert!(again.is_empty(), "nothing changed: {again:?}");

    // Something the user did in the worktree, which is exactly what the
    // poller is for.
    std::fs::write(workspace.worktree.path.join("scratch.txt"), "work\n").unwrap();
    fixture.service.poll_statuses();
    let dirty: Vec<_> = fixture
        .recorder
        .taken()
        .into_iter()
        .filter(|event| matches!(event, DaemonEvent::WorkspaceStatusChanged { .. }))
        .collect();
    assert_eq!(dirty.len(), 1, "{dirty:?}");
}

#[test]
fn a_workspace_pull_request_reaches_the_listing_and_is_pushed_only_when_it_changes() {
    use ginka_protocol::model::{PullRequest, PullRequestState};
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "reviewed".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let id = workspace.id();
    // The fixture's repository has no remote, so the poller would not ask.
    assert!(fixture.service.pull_request_plan().is_empty());
    fixture.recorder.taken();

    let pushed = |fixture: &mut Fixture| -> Vec<DaemonEvent> {
        fixture
            .recorder
            .taken()
            .into_iter()
            .filter(|event| matches!(event, DaemonEvent::WorkspacePullRequestChanged { .. }))
            .collect()
    };
    let open = PullRequest {
        number: 7,
        url: "https://github.com/o/r/pull/7".into(),
        state: PullRequestState::Open,
    };
    fixture
        .service
        .set_pull_requests(vec![(id.clone(), Some(open.clone()))]);
    assert_eq!(pushed(&mut fixture).len(), 1);
    fixture
        .service
        .set_pull_requests(vec![(id.clone(), Some(open.clone()))]);
    assert!(pushed(&mut fixture).is_empty(), "nothing changed");

    let listed = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    let row = listed.iter().find(|summary| summary.id() == id).unwrap();
    assert_eq!(row.pull_request.as_ref(), Some(&open));

    let merged = PullRequest {
        state: PullRequestState::Merged,
        ..open
    };
    fixture
        .service
        .set_pull_requests(vec![(id.clone(), Some(merged))]);
    assert_eq!(pushed(&mut fixture).len(), 1);
    fixture.service.set_pull_requests(vec![(id.clone(), None)]);
    assert_eq!(
        pushed(&mut fixture).len(),
        1,
        "a pull request that went away"
    );
}

#[test]
fn a_history_row_opens_what_that_commit_did_and_nothing_else_is_taken_as_one() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "graph".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let path = workspace.worktree.path.clone();
    std::fs::write(path.join("added.txt"), "one\ntwo\n").unwrap();
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "Add a file",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&path)
                .status()
                .unwrap()
                .success()
        );
    }
    let commits = match fixture.ask(Request::WorkspaceHistory {
        workspace: workspace.id(),
        limit: Some(5),
    }) {
        Response::History { commits } => commits,
        other => panic!("expected history, got {other:?}"),
    };
    let changes = match fixture.ask(Request::WorkspaceChanges {
        workspace: workspace.id(),
        source: ChangeSource::Commit {
            commit: commits[0].id.clone(),
        },
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(changes.files.len(), 1);
    assert_eq!(changes.files[0].path, "added.txt");
    assert_eq!(changes.files[0].added, 2);

    // Anything but a commit id is refused before git sees it.
    assert!(
        fixture
            .service
            .handle(Request::WorkspaceChanges {
                workspace: workspace.id(),
                source: ChangeSource::Commit {
                    commit: "--output=/tmp/ginka-pwned".into(),
                },
            })
            .is_err()
    );
}

#[test]
fn notes_are_kept_per_project_and_survive_an_edit() {
    let mut fixture = Fixture::new();
    let project = ProjectName("comet".into());
    let note = match fixture.ask(Request::SaveNote {
        id: None,
        project: Some(project.clone()),
        title: String::new(),
        body: "# Release\nrun the script".into(),
        tags: Some(vec!["deploy".into()]),
    }) {
        Response::Note { note } => note,
        other => panic!("expected a note, got {other:?}"),
    };
    assert_eq!(note.title, "Release", "an untitled note is its first line");
    fixture.ask(Request::SaveNote {
        id: Some(note.id.clone()),
        project: None,
        title: "Release steps".into(),
        body: "changed".into(),
        tags: None,
    });
    match fixture.ask(Request::ListNotes {
        project: Some(project),
        query: Some("DEPLOY".into()),
        tag: Some("deploy".into()),
    }) {
        Response::Notes { notes } => {
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].title, "Release steps");
            assert_eq!(notes[0].body, "changed");
            assert_eq!(notes[0].tags, vec!["deploy"]);
        }
        other => panic!("expected notes, got {other:?}"),
    }
    fixture.ask(Request::RemoveNote { id: note.id });
    match fixture.ask(Request::ListNotes {
        project: None,
        query: None,
        tag: None,
    }) {
        Response::Notes { notes } => assert!(notes.is_empty()),
        other => panic!("expected notes, got {other:?}"),
    }
}

#[test]
fn a_quick_command_runs_in_a_terminal_named_after_it() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "quick".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let shell = match fixture.ask(Request::SaveQuickCommand {
        id: None,
        project: Some(project.clone()),
        name: "mark".into(),
        kind: ginka_protocol::model::QuickCommandKind::Shell,
        body: "echo quick > .quick-ran".into(),
    }) {
        Response::QuickCommand { command } => command,
        other => panic!("expected a command, got {other:?}"),
    };
    let prompt = match fixture.ask(Request::SaveQuickCommand {
        id: None,
        project: None,
        name: "Review".into(),
        kind: ginka_protocol::model::QuickCommandKind::Prompt,
        body: "review the diff".into(),
    }) {
        Response::QuickCommand { command } => command,
        other => panic!("expected a command, got {other:?}"),
    };
    match fixture.ask(Request::ListQuickCommands {
        project: Some(project),
    }) {
        Response::QuickCommands { commands } => assert_eq!(commands.len(), 2),
        other => panic!("expected commands, got {other:?}"),
    }

    fixture.ask(Request::RunQuickCommand {
        workspace: workspace.id(),
        id: shell.id,
        rows: 24,
        cols: 80,
    });
    let worktree = workspace.worktree.path.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !worktree.join(".quick-ran").is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "the command never ran"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    match fixture.ask(Request::WorkspaceTerminals {
        workspace: workspace.id(),
    }) {
        Response::Terminals { terminals } => {
            assert!(terminals.iter().any(|terminal| terminal.title == "mark"))
        }
        other => panic!("expected terminals, got {other:?}"),
    }
    assert!(
        fixture
            .service
            .handle(Request::RunQuickCommand {
                workspace: workspace.id(),
                id: prompt.id,
                rows: 24,
                cols: 80,
            })
            .is_err(),
        "a prompt is the conversation's to send"
    );
}

#[test]
fn the_winning_attempt_is_merged_into_the_branch_the_project_is_on() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "try-1".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    // Agents leave their work uncommitted.
    std::fs::write(workspace.worktree.path.join("answer.txt"), "42\n").unwrap();

    // Without a message there is no way to carry that work over: refused.
    assert!(
        fixture
            .service
            .handle(Request::MergeWorkspace {
                workspace: workspace.id(),
                into: None,
                message: None,
            })
            .is_err()
    );
    assert!(!fixture.repo().join("answer.txt").exists());

    let outcome = match fixture.ask(Request::MergeWorkspace {
        workspace: workspace.id(),
        into: None,
        message: Some("Keep try-1".into()),
    }) {
        Response::Merged { outcome } => outcome,
        other => panic!("expected a merge, got {other:?}"),
    };
    assert_eq!(outcome.into, "main", "the branch the project is on");
    assert!(outcome.fast_forward);
    assert_eq!(
        std::fs::read_to_string(fixture.repo().join("answer.txt")).unwrap(),
        "42\n"
    );
    assert_eq!(
        support::git(&fixture.repo(), &["log", "-1", "--format=%s"]).trim(),
        "Keep try-1"
    );

    // A workspace cannot be merged into its own branch.
    assert!(
        fixture
            .service
            .handle(Request::MergeWorkspace {
                workspace: workspace.id(),
                into: Some("try-1".into()),
                message: None,
            })
            .is_err()
    );
}

/// Save a scheduled job and take it back.
fn save_cron(fixture: &mut Fixture, request: Request) -> ginka_protocol::model::CronJob {
    match fixture.ask(request) {
        Response::CronJob { job } => job,
        other => panic!("expected a job, got {other:?}"),
    }
}

fn cron_runs(fixture: &mut Fixture, id: i64) -> Vec<ginka_protocol::model::CronRun> {
    match fixture.ask(Request::CronRuns { id, limit: None }) {
        Response::CronRuns { runs } => runs,
        other => panic!("expected runs, got {other:?}"),
    }
}

fn terminal_job(project: &ProjectName, schedule: &str, body: &str) -> Request {
    Request::SaveCronJob {
        id: None,
        project: project.clone(),
        workspace: None,
        name: "nightly".into(),
        schedule: schedule.into(),
        via: ginka_protocol::model::CronVia::Terminal,
        agent: None,
        body: body.into(),
        precheck: None,
        enabled: true,
    }
}

#[test]
fn a_scheduled_job_is_checked_when_it_is_written() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();

    for bad in [
        terminal_job(&project, "61 * * * *", "true"),
        terminal_job(&project, "0 0 31 2 *", "true"),
        terminal_job(&ProjectName("nowhere".into()), "@daily", "true"),
        terminal_job(&project, "@daily", "  "),
        Request::SaveCronJob {
            id: None,
            project: project.clone(),
            workspace: None,
            name: "review".into(),
            schedule: "@daily".into(),
            via: ginka_protocol::model::CronVia::Chat,
            agent: None,
            body: "review the diff".into(),
            precheck: None,
            enabled: true,
        },
    ] {
        assert!(fixture.service.handle(bad.clone()).is_err(), "{bad:?}");
    }

    let job = save_cron(&mut fixture, terminal_job(&project, "@daily", "true"));
    let now = chrono::Utc::now().timestamp();
    let next = job.next_run_at.expect("an enabled job says when it fires");
    assert!(next > now && next <= now + 86_400, "{next} vs {now}");

    let disabled = save_cron(
        &mut fixture,
        Request::SaveCronJob {
            id: Some(job.id),
            project: project.clone(),
            workspace: None,
            name: "nightly".into(),
            schedule: "@daily".into(),
            via: ginka_protocol::model::CronVia::Terminal,
            agent: None,
            body: "true".into(),
            precheck: None,
            enabled: false,
        },
    );
    assert_eq!(disabled.id, job.id, "saved in place");
    assert_eq!(disabled.next_run_at, None);

    match fixture.ask(Request::ListCronJobs {
        project: Some(project),
    }) {
        Response::CronJobs { jobs } => assert_eq!(jobs.len(), 1),
        other => panic!("expected jobs, got {other:?}"),
    }
    fixture.ask(Request::RemoveCronJob { id: job.id });
    match fixture.ask(Request::ListCronJobs { project: None }) {
        Response::CronJobs { jobs } => assert!(jobs.is_empty()),
        other => panic!("expected jobs, got {other:?}"),
    }
}

#[test]
fn a_due_terminal_job_runs_once_and_is_skipped_while_it_is_still_running() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let marker = fixture.repo().join(".cron-ran");
    let job = save_cron(
        &mut fixture,
        terminal_job(&project, "* * * * *", "echo ran >> .cron-ran; sleep 30"),
    );

    // Nothing is owed before the schedule's next minute.
    assert_eq!(fixture.service.run_due_cron(chrono::Local::now()), 0);

    let later = chrono::Local::now() + chrono::Duration::minutes(2);
    assert_eq!(fixture.service.run_due_cron(later), 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !marker.is_file() {
        assert!(std::time::Instant::now() < deadline, "the job never ran");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    // Missed minutes are not replayed: the tick that fired moved it on.
    assert_eq!(fixture.service.run_due_cron(later), 0);

    // Its command is still sleeping, so the next firing is skipped.
    let even_later = later + chrono::Duration::minutes(2);
    assert_eq!(fixture.service.run_due_cron(even_later), 1);
    let runs = cron_runs(&mut fixture, job.id);
    let outcomes: Vec<_> = runs.iter().map(|run| run.outcome).collect();
    assert_eq!(
        outcomes,
        [
            ginka_protocol::model::CronOutcome::Skipped,
            ginka_protocol::model::CronOutcome::Started,
        ],
        "most recent first"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ran\n");

    match fixture.ask(Request::ListCronJobs { project: None }) {
        Response::CronJobs { jobs } => assert_eq!(
            jobs[0].last_run.as_ref().map(|run| run.outcome),
            Some(ginka_protocol::model::CronOutcome::Skipped)
        ),
        other => panic!("expected jobs, got {other:?}"),
    }
}

#[test]
fn a_terminal_job_is_finished_when_its_terminal_closes_and_can_be_run_by_hand() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let job = save_cron(&mut fixture, terminal_job(&project, "@yearly", "true"));

    fixture.ask(Request::RunCronJob { id: job.id });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let runs = cron_runs(&mut fixture, job.id);
        if runs[0].outcome == ginka_protocol::model::CronOutcome::Finished {
            assert!(runs[0].finished_at.is_some());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never finished: {runs:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        fixture
            .service
            .handle(Request::RunCronJob { id: 999 })
            .is_err()
    );
}

fn project_names(fixture: &mut Fixture) -> Vec<(String, Option<String>)> {
    match fixture.ask(Request::ListProjects) {
        Response::Projects { projects } => projects
            .into_iter()
            .map(|project| (project.name.0, project.label))
            .collect(),
        other => panic!("expected projects, got {other:?}"),
    }
}

#[test]
fn projects_keep_the_order_and_labels_the_reader_gives_them() {
    let mut fixture = Fixture::new();
    for name in ["comet", "aurora", "nebula"] {
        let root = fixture.work.path().join(name);
        support::repository(&root);
        fixture.ask(Request::AddProject {
            path: root,
            label: None,
        });
    }
    // A new project goes to the end rather than into the alphabet.
    let names: Vec<String> = project_names(&mut fixture)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["comet", "aurora", "nebula"]);

    fixture.ask(Request::MoveProject {
        project: ProjectName("nebula".into()),
        index: 0,
    });
    fixture.ask(Request::MoveProject {
        project: ProjectName("comet".into()),
        index: 99,
    });
    let names: Vec<String> = project_names(&mut fixture)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["nebula", "aurora", "comet"],
        "a far index is the end"
    );

    fixture.ask(Request::SetProjectLabel {
        project: ProjectName("aurora".into()),
        label: "Work".into(),
    });
    assert_eq!(
        project_names(&mut fixture)[1],
        ("aurora".to_string(), Some("Work".to_string()))
    );
    fixture.ask(Request::SetProjectLabel {
        project: ProjectName("aurora".into()),
        label: "  ".into(),
    });
    assert_eq!(project_names(&mut fixture)[1].1, None, "blank clears it");

    assert!(
        fixture
            .service
            .handle(Request::MoveProject {
                project: ProjectName("nowhere".into()),
                index: 0,
            })
            .is_err()
    );
}

fn new_workspace(
    fixture: &mut Fixture,
    project: &ProjectName,
    branch: &str,
) -> Result<ginka_protocol::model::WorkspaceSummary, String> {
    match fixture.service.handle(Request::CreateWorkspace {
        project: project.clone(),
        branch: branch.into(),
        base: None,
    }) {
        Ok(Response::Workspace { workspace }) => Ok(workspace),
        Ok(other) => panic!("expected a workspace, got {other:?}"),
        Err(error) => Err(error.message),
    }
}

#[test]
fn a_worktree_deleted_by_hand_does_not_hold_its_branch_hostage() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = new_workspace(&mut fixture, &project, "doomed").unwrap();
    // Deleted outside the app, the way a cleanup script or a reader would.
    std::fs::remove_dir_all(&workspace.worktree.path).unwrap();

    fixture.ask(Request::ListWorkspaces {
        project: Some(project.clone()),
    });
    let listed = support::git(&fixture.repo(), &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains("prunable"),
        "git's record is gone too: {listed}"
    );

    // The branch is free again: a new workspace can take it.
    new_workspace(&mut fixture, &project, "doomed").expect("the branch is not held");
}

#[test]
fn a_locked_worktree_is_named_as_locked_and_removed_only_when_forced() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = new_workspace(&mut fixture, &project, "pinned-down").unwrap();
    support::git(
        &fixture.repo(),
        &[
            "worktree",
            "lock",
            "--reason",
            "on a USB disk",
            workspace.worktree.path.to_str().unwrap(),
        ],
    );

    let refused = fixture
        .service
        .handle(Request::RemoveWorkspace {
            workspace: workspace.id(),
            force: false,
        })
        .unwrap_err();
    assert!(refused.message.contains("locked"), "{}", refused.message);
    assert!(
        refused.message.contains("on a USB disk"),
        "{}",
        refused.message
    );
    assert!(workspace.worktree.path.exists());

    fixture.ask(Request::RemoveWorkspace {
        workspace: workspace.id(),
        force: true,
    });
    assert!(
        !workspace.worktree.path.exists(),
        "force goes through the lock"
    );
}

#[test]
fn a_settings_file_edited_by_hand_is_read_again_without_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(home.path().join("state"));
    paths.ensure().unwrap();
    let mut service = Service::new(
        paths.clone(),
        db::open_in_memory().unwrap(),
        Arc::new(Recorder::default()),
    );
    service
        .handle(Request::AddAccount {
            id: ginka_protocol::AccountId("codex-work".into()),
            provider: ginka_protocol::ProviderKind::Codex,
            label: "Work".into(),
        })
        .unwrap();
    // Nothing has changed since the service wrote the file itself.
    assert!(!service.reload_settings_if_changed());

    // The reader edits the file in their editor.
    let file = paths.daemon_settings();
    let mut settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    settings["accounts"]["codex-work"]["label"] = "Day job".into();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&file, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

    assert!(service.reload_settings_if_changed());
    let accounts = match service.handle(Request::Accounts).unwrap() {
        Response::Accounts { accounts } => accounts,
        other => panic!("expected accounts, got {other:?}"),
    };
    let work = accounts
        .iter()
        .find(|account| account.id.0 == "codex-work")
        .unwrap();
    assert_eq!(work.label, "Day job");
    assert!(!service.reload_settings_if_changed(), "read once");

    // A file that no longer parses is not taken: the running settings stay.
    std::fs::write(&file, "{ not json").unwrap();
    assert!(!service.reload_settings_if_changed());
}

#[test]
fn reports_count_what_agents_spent_outside_ginka_by_model_and_by_project() {
    let logs = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    // An agent's working directory as it reports it: the real path, as the
    // project is registered by.
    let folder = fixture.repo().canonicalize().unwrap();
    let claude = logs.path().join("projects/-work");
    std::fs::create_dir_all(&claude).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let turn = |id: &str| {
        format!(
            r#"{{"type":"assistant","sessionId":"terminal-1","cwd":"{}","timestamp":"{now}","message":{{"id":"{id}","model":"claude-opus-5-5","usage":{{"input_tokens":1000,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#,
            folder.display()
        )
    };
    std::fs::write(claude.join("s.jsonl"), format!("{}\n", turn("m1"))).unwrap();
    fixture.service = std::mem::replace(
        &mut fixture.service,
        Service::new(
            Paths::with_root(logs.path().join("unused")),
            db::open_in_memory().unwrap(),
            Arc::new(Recorder::default()),
        ),
    )
    .with_vendor_logs(vec![(
        logs.path().join("projects"),
        ginka_core::usage::scan::Format::Claude,
    )]);

    assert_eq!(fixture.service.scan_outside_now(), 1);
    // Scanning again reads only what was appended.
    assert_eq!(fixture.service.scan_outside_now(), 0);
    std::fs::write(
        claude.join("s.jsonl"),
        format!("{}\n{}\n", turn("m1"), turn("m2")),
    )
    .unwrap();
    assert_eq!(fixture.service.scan_outside_now(), 1);

    let (by_model, by_project) = match fixture.ask(Request::Usage { days: Some(30) }) {
        Response::Usage {
            by_model,
            by_project,
            ..
        } => (by_model, by_project),
        other => panic!("expected usage, got {other:?}"),
    };
    let model = by_model
        .iter()
        .find(|row| row.label == "claude-opus-5-5")
        .expect("the model is listed");
    assert_eq!(model.totals.input_tokens, 2_000);
    let comet = by_project
        .iter()
        .find(|row| row.label == project.0)
        .expect("the run in the project's folder is the project's");
    assert_eq!(comet.totals.input_tokens, 2_000);
}

#[test]
fn ginkas_own_skills_are_installed_where_the_agents_read_them() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new();
    fixture.service = std::mem::replace(
        &mut fixture.service,
        Service::new(
            Paths::with_root(home.path().join("unused")),
            db::open_in_memory().unwrap(),
            Arc::new(Recorder::default()),
        ),
    )
    .with_skills_home(home.path());

    let installed = match fixture.ask(Request::InstallBundledSkills { force: false }) {
        Response::BundledSkillsInstalled { results } => results,
        other => panic!("expected results, got {other:?}"),
    };
    // Four skills into Claude Code's directory and Codex's.
    assert_eq!(installed.len(), 8, "{installed:?}");
    assert!(installed.iter().all(|result| result.outcome == "written"));
    assert!(
        home.path()
            .join(".claude/skills/ginka-loop/SKILL.md")
            .is_file()
    );
    assert!(
        home.path()
            .join(".codex/skills/ginka-start/SKILL.md")
            .is_file()
    );

    let listed = match fixture.ask(Request::ListSkills { project: None }) {
        Response::Skills { skills, .. } => skills,
        other => panic!("expected skills, got {other:?}"),
    };
    assert!(
        listed.iter().any(|skill| skill.name == "ginka-chat"),
        "and listed"
    );

    match fixture.ask(Request::InstallBundledSkills { force: false }) {
        Response::BundledSkillsInstalled { results } => {
            assert!(results.iter().all(|result| result.outcome == "unchanged"))
        }
        other => panic!("expected results, got {other:?}"),
    }
}

#[test]
fn the_address_bar_completes_from_the_workspaces_own_history() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = WorkspaceId(format!("{}/main", project.0));
    for url in [
        "http://localhost:3000/dashboard",
        "http://localhost:3000/dashboard",
        "https://user:pw@example.com/login?token=secret",
    ] {
        fixture.ask(Request::RecordBrowserVisit {
            workspace: workspace.clone(),
            url: url.into(),
            title: Some("Page".into()),
        });
    }
    let pages = match fixture.ask(Request::BrowserSuggestions {
        workspace: workspace.clone(),
        query: "".into(),
        limit: None,
    }) {
        Response::BrowserSuggestions { pages } => pages,
        other => panic!("expected pages, got {other:?}"),
    };
    let urls: Vec<&str> = pages.iter().map(|page| page.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "http://localhost:3000/dashboard",
            "https://example.com/login"
        ],
        "most visited first, and nothing secret kept"
    );
    assert_eq!(pages[0].visits, 2);
}

#[test]
fn the_daemons_settings_are_shown_without_secrets_and_changed_by_key() {
    let mut fixture = Fixture::new();
    let shown = |fixture: &mut Fixture| -> serde_json::Value {
        match fixture.ask(Request::DaemonSettings) {
            Response::DaemonSettings { json } => serde_json::from_str(&json).unwrap(),
            other => panic!("expected settings, got {other:?}"),
        }
    };
    assert_eq!(shown(&mut fixture)["keep_awake"], true);

    fixture.ask(Request::UpdateDaemonSettings {
        key: "keep_awake".into(),
        value: "false".into(),
    });
    fixture.ask(Request::UpdateDaemonSettings {
        key: "agents".into(),
        value: r#"{"claude": {"env": {"ANTHROPIC_API_KEY": "sk-secret"}}}"#.into(),
    });
    let now = shown(&mut fixture);
    assert_eq!(now["keep_awake"], false);
    assert_eq!(
        now["agents"]["claude"]["env"]["ANTHROPIC_API_KEY"], "[set]",
        "a value is never shown, only that it is set"
    );
    assert!(!now.to_string().contains("sk-secret"));

    for (key, value) in [
        ("keep_awak", "false"),    // a typo
        ("keep_awake", "\"yes\""), // the wrong type
        ("retention_days", "not json at all"),
    ] {
        assert!(
            fixture
                .service
                .handle(Request::UpdateDaemonSettings {
                    key: key.into(),
                    value: value.into(),
                })
                .is_err(),
            "{key} = {value}"
        );
    }
    assert_eq!(
        shown(&mut fixture)["keep_awake"],
        false,
        "a refusal changes nothing"
    );
}

#[test]
fn a_conversation_started_in_the_claude_cli_can_be_adopted_once() {
    let claude = tempfile::tempdir().unwrap();
    let roots = ginka_core::cli_sessions::Roots {
        claude: Some(claude.path().to_path_buf()),
        codex: None,
    };
    let mut fixture = Fixture::configured(|service| service.with_cli_roots(roots));
    let folder = fixture.work.path().join("notes");
    std::fs::create_dir_all(&folder).unwrap();
    let project = match fixture.ask(Request::AddProject {
        path: folder,
        label: None,
    }) {
        Response::Project { project } => project,
        other => panic!("expected a project, got {other:?}"),
    };
    let workspace = match fixture.ask(Request::ListWorkspaces {
        project: Some(project.name.clone()),
    }) {
        Response::Workspaces { workspaces } => workspaces.into_iter().next().unwrap(),
        other => panic!("expected workspaces, got {other:?}"),
    };
    let cwd = workspace.worktree.path.to_string_lossy().into_owned();

    // What `claude` wrote when it was run in that folder from a terminal.
    let file = claude
        .path()
        .join("projects")
        .join(ginka_core::cli_sessions::claude_project_dir(
            &workspace.worktree.path,
        ))
        .join("c-1.jsonl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let line = |kind: &str, content: serde_json::Value| {
        serde_json::json!({"type": kind, "sessionId": "c-1", "cwd": cwd,
            "timestamp": "2026-09-26T01:00:00Z",
            "message": {"id": "m1", "role": kind, "content": content}})
        .to_string()
    };
    std::fs::write(
        &file,
        [
            line("user", serde_json::json!("Tidy the notes")),
            line(
                "assistant",
                serde_json::json!([{"type": "text", "text": "Tidied."}]),
            ),
        ]
        .join("\n"),
    )
    .unwrap();
    let listed = match fixture.ask(Request::CliSessions {
        workspace: workspace.id(),
    }) {
        Response::CliSessions { sessions } => sessions,
        other => panic!("expected CLI sessions, got {other:?}"),
    };
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Tidy the notes");

    let adopted = match fixture.ask(Request::AdoptCliSession {
        workspace: workspace.id(),
        agent: "claude".into(),
        vendor_session_id: "c-1".into(),
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    // Its next turn resumes the CLI's thread, on the login that wrote it.
    assert_eq!(adopted.vendor_session_id.as_deref(), Some("c-1"));
    assert_eq!(adopted.account.0, "claude");
    assert_eq!(adopted.workspace, workspace.id());
    assert!(fixture.recorder.taken().iter().any(|event| matches!(
        event,
        DaemonEvent::SessionStarted { session } if session.id == adopted.id
    )));

    let entries = match fixture.ask(Request::SessionTranscript {
        session: adopted.id.clone(),
        after: None,
        limit: None,
    }) {
        Response::Transcript { entries } => entries,
        other => panic!("expected a transcript, got {other:?}"),
    };
    assert_eq!(entries.len(), 3, "prompt, reply and the turn's end");

    // Held now, so neither offered nor adoptable again.
    match fixture.ask(Request::CliSessions {
        workspace: workspace.id(),
    }) {
        Response::CliSessions { sessions } => assert!(sessions.is_empty()),
        other => panic!("expected CLI sessions, got {other:?}"),
    }
    assert!(
        fixture
            .service
            .handle(Request::AdoptCliSession {
                workspace: workspace.id(),
                agent: "claude".into(),
                vendor_session_id: "c-1".into(),
            })
            .is_err()
    );
}

#[test]
fn an_agent_writes_its_status_from_inside_its_worktree() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "status".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let inside = workspace.worktree.path.join("src");
    std::fs::create_dir_all(&inside).unwrap();

    fixture.ask(Request::SetWorkspaceStatus {
        workspace: None,
        path: Some(inside),
        note: Some("tests green, writing docs".into()),
    });
    let listed = match fixture.ask(Request::ListWorkspaces {
        project: Some(project.clone()),
    }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    let row = listed
        .iter()
        .find(|summary| summary.id() == workspace.id())
        .unwrap();
    assert_eq!(
        row.status_note.as_ref().map(|note| note.text.as_str()),
        Some("tests green, writing docs")
    );
    let main = listed
        .iter()
        .find(|summary| summary.id() != workspace.id())
        .unwrap();
    assert_eq!(
        main.status_note, None,
        "the project's own checkout holds the worktree directory, but the innermost worktree wins"
    );

    fixture.ask(Request::SetWorkspaceStatus {
        workspace: Some(workspace.id()),
        path: None,
        note: None,
    });
    let cleared = match fixture.ask(Request::ListWorkspaces {
        project: Some(project),
    }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(cleared.iter().all(|summary| summary.status_note.is_none()));
}

#[test]
fn a_failing_precheck_skips_the_firing_and_says_why() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let marker = fixture.repo().join(".cron-ran");
    let mut request = terminal_job(&project, "* * * * *", "echo ran >> .cron-ran");
    if let Request::SaveCronJob { precheck, .. } = &mut request {
        *precheck = Some("test -f .go || { echo nothing new; exit 1; }".into());
    }
    let job = save_cron(&mut fixture, request);
    assert_eq!(
        job.precheck.as_deref(),
        Some("test -f .go || { echo nothing new; exit 1; }")
    );

    let later = chrono::Local::now() + chrono::Duration::minutes(2);
    assert_eq!(fixture.service.run_due_cron(later), 1);
    let runs = cron_runs(&mut fixture, job.id);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, ginka_protocol::model::CronOutcome::Skipped);
    assert_eq!(
        runs[0].detail.as_deref(),
        Some("precheck exited 1: nothing new")
    );
    assert!(!marker.exists(), "a skipped firing starts nothing");

    std::fs::write(fixture.repo().join(".go"), "").unwrap();
    let even_later = later + chrono::Duration::minutes(2);
    assert_eq!(fixture.service.run_due_cron(even_later), 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !marker.is_file() {
        assert!(std::time::Instant::now() < deadline, "the job never ran");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
