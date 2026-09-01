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
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(home.path().join("state"));
        paths.ensure().unwrap();
        let recorder = Arc::new(Recorder::default());
        let service = Service::new(paths, db::open_in_memory().unwrap(), recorder.clone());
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
        match self.ask(Request::AddProject { path: root }) {
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
fn a_plain_folder_is_a_project_with_one_implicit_workspace() {
    let mut fixture = Fixture::new();
    let notes = fixture.work.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let project = match fixture.ask(Request::AddProject { path: notes.clone() }) {
        Response::Project { project } => project,
        other => panic!("expected a project, got {other:?}"),
    };
    assert_eq!(project.kind, ginka_protocol::ProjectKind::Plain);

    // Listing must not fail on a folder git knows nothing about.
    match fixture.ask(Request::ListWorkspaces {
        project: Some(project.name),
    }) {
        Response::Workspaces { workspaces } => assert!(workspaces.is_empty()),
        other => panic!("expected workspaces, got {other:?}"),
    }
}

#[test]
fn the_service_reports_where_its_state_lives() {
    let fixture = Fixture::new();
    assert!(fixture.service.paths().root().ends_with("state"));
    assert!(Path::new(fixture.service.paths().root()).is_dir());
}
