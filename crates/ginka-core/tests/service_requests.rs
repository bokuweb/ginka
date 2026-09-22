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
fn one_hunk_can_cross_the_service_boundary_without_staging_the_whole_file() {
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
    let edited = baseline
        .replace("line 2\n", "line two\n")
        .replace("line 29\n", "line twenty-nine\n");
    std::fs::write(workspace.worktree.path.join("README.md"), edited).unwrap();

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
        header: first_header,
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
        workspace: id,
        source: ChangeSource::Unstaged,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert!(staged.is_empty());
    assert_eq!(unstaged.files[0].hunks.len(), 2);
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
    assert!(
        worktree.join(".setup-ran").is_file(),
        "the setup command ran in the new worktree"
    );
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
