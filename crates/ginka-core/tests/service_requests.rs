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
fn workspace_folders_survive_git_reconciliation_and_archive_without_rekeying() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let original = new_workspace(&mut fixture, &project, "topic").unwrap();
    let id = original.id();
    fixture.recorder.taken();
    assert_eq!(
        fixture.ask(Request::SetWorkspaceFolder {
            workspace: id.clone(),
            folder: Some("  Review  ".into()),
        }),
        Response::Ack
    );
    assert_eq!(
        fixture.recorder.taken(),
        vec![DaemonEvent::WorkspacesChanged {
            project: project.clone(),
        }]
    );
    assert!(
        fixture
            .service
            .handle(Request::SetWorkspaceFolder {
                workspace: id.clone(),
                folder: Some("bad\nname".into()),
            })
            .is_err()
    );
    assert!(fixture.recorder.taken().is_empty());
    fixture.ask(Request::PinWorkspace {
        workspace: id.clone(),
        pinned: true,
    });
    fixture.ask(Request::ArchiveWorkspace {
        workspace: id.clone(),
        archived: true,
    });
    support::git(&original.worktree.path, &["checkout", "-b", "renamed"]);
    let Response::Workspaces { workspaces } = fixture.ask(Request::ListWorkspaces {
        project: Some(project.clone()),
    }) else {
        panic!("expected workspaces")
    };
    let workspace = workspaces.iter().find(|row| row.id() == id).unwrap();
    assert_eq!(workspace.worktree.folder.as_deref(), Some("Review"));
    assert_eq!(workspace.worktree.name, original.worktree.name);
    assert_eq!(workspace.worktree.path, original.worktree.path);
    assert_eq!(workspace.worktree.branch, "renamed");
    assert!(workspace.worktree.archived && workspace.worktree.pinned);
    assert!(
        workspaces
            .iter()
            .filter(|row| row.id() != id)
            .all(|row| row.worktree.folder.is_none())
    );
    fixture.ask(Request::SetWorkspaceFolder {
        workspace: id.clone(),
        folder: None,
    });
    let Response::Workspaces { workspaces } =
        fixture.ask(Request::ListWorkspaces { project: None })
    else {
        panic!("expected workspaces")
    };
    assert!(
        workspaces
            .iter()
            .find(|row| row.id() == id)
            .unwrap()
            .worktree
            .folder
            .is_none()
    );
    assert!(
        fixture
            .service
            .handle(Request::SetWorkspaceFolder {
                workspace: WorkspaceId("comet/missing".into()),
                folder: Some("Review".into()),
            })
            .is_err()
    );
}

#[test]
fn batch_folders_emit_once_after_commit_and_never_on_a_rejected_batch() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let a = new_workspace(&mut fixture, &project, "batch-a").unwrap();
    let b = new_workspace(&mut fixture, &project, "batch-b").unwrap();
    fixture.ask(Request::ArchiveWorkspace {
        workspace: b.id(),
        archived: true,
    });
    fixture.recorder.taken();
    assert!(
        fixture
            .service
            .handle(Request::SetWorkspaceFolders {
                project: project.clone(),
                workspaces: vec![a.id(), WorkspaceId("comet/missing".into())],
                folder: Some("Review".into()),
            })
            .is_err()
    );
    assert!(fixture.recorder.taken().is_empty());
    assert_eq!(
        fixture.ask(Request::SetWorkspaceFolders {
            project: project.clone(),
            workspaces: vec![a.id(), b.id(), a.id()],
            folder: Some("Review".into()),
        }),
        Response::Ack
    );
    assert_eq!(
        fixture.recorder.taken(),
        vec![DaemonEvent::WorkspacesChanged {
            project: project.clone()
        }]
    );
    let Response::Workspaces { workspaces } = fixture.ask(Request::ListWorkspaces {
        project: Some(project),
    }) else {
        panic!("expected workspaces")
    };
    for original in [a, b] {
        let row = workspaces
            .iter()
            .find(|row| row.id() == original.id())
            .unwrap();
        assert_eq!(row.worktree.folder.as_deref(), Some("Review"));
        assert_eq!(row.worktree.path, original.worktree.path);
        assert_eq!(row.worktree.branch, original.worktree.branch);
    }
}

#[test]
fn batch_pins_publish_one_committed_update_and_keep_other_metadata() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let a = new_workspace(&mut fixture, &project, "pin-a").unwrap();
    let b = new_workspace(&mut fixture, &project, "pin-b").unwrap();
    fixture.ask(Request::PinWorkspace {
        workspace: b.id(),
        pinned: true,
    });
    fixture.ask(Request::ArchiveWorkspace {
        workspace: b.id(),
        archived: true,
    });
    fixture.ask(Request::SetWorkspaceFolder {
        workspace: a.id(),
        folder: Some("Review".into()),
    });
    let Response::Workspaces { workspaces: before } = fixture.ask(Request::ListWorkspaces {
        project: Some(project.clone()),
    }) else {
        panic!("expected workspaces")
    };
    fixture.recorder.taken();
    assert!(
        fixture
            .service
            .handle(Request::PinWorkspaces {
                project: project.clone(),
                workspaces: vec![a.id(), WorkspaceId("comet/missing".into())],
                pinned: true
            })
            .is_err()
    );
    assert!(fixture.recorder.taken().is_empty());
    for pinned in [true, false] {
        assert_eq!(
            fixture.ask(Request::PinWorkspaces {
                project: project.clone(),
                workspaces: vec![a.id(), b.id(), a.id()],
                pinned
            }),
            Response::Ack
        );
        assert_eq!(
            fixture.recorder.taken(),
            vec![DaemonEvent::WorkspacesChanged {
                project: project.clone()
            }]
        );
        let Response::Workspaces { workspaces } = fixture.ask(Request::ListWorkspaces {
            project: Some(project.clone()),
        }) else {
            panic!("expected workspaces")
        };
        for original in &before {
            let row = workspaces
                .iter()
                .find(|row| row.id() == original.id())
                .unwrap();
            let mut expected = original.worktree.clone();
            if [a.id(), b.id()].contains(&original.id()) {
                expected.pinned = pinned;
            }
            assert_eq!(row.worktree, expected);
        }
    }
}

#[test]
fn batch_archives_publish_one_committed_update_and_keep_other_metadata() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let a = new_workspace(&mut fixture, &project, "archive-a").unwrap();
    let b = new_workspace(&mut fixture, &project, "archive-b").unwrap();
    let paths = [Path::new(&a.worktree.path), Path::new(&b.worktree.path)];
    std::fs::write(paths[0].join("README.md"), "uncommitted tracked edit\n").unwrap();
    std::fs::write(paths[1].join("archive-notes.txt"), "untracked notes\n").unwrap();
    let git_before: Vec<_> = paths
        .iter()
        .map(|path| {
            (
                support::git(path, &["rev-parse", "HEAD"]),
                support::git(path, &["status", "--porcelain"]),
            )
        })
        .collect();
    fixture.ask(Request::PinWorkspace {
        workspace: b.id(),
        pinned: true,
    });
    fixture.ask(Request::ArchiveWorkspace {
        workspace: b.id(),
        archived: true,
    });
    fixture.ask(Request::SetWorkspaceFolder {
        workspace: a.id(),
        folder: Some("Review".into()),
    });
    let Response::Workspaces { workspaces: before } = fixture.ask(Request::ListWorkspaces {
        project: Some(project.clone()),
    }) else {
        panic!("expected workspaces")
    };
    fixture.recorder.taken();
    assert!(
        fixture
            .service
            .handle(Request::ArchiveWorkspaces {
                project: project.clone(),
                workspaces: vec![a.id(), WorkspaceId("comet/missing".into())],
                archived: true
            })
            .is_err()
    );
    assert!(fixture.recorder.taken().is_empty());
    for archived in [true, false] {
        assert_eq!(
            fixture.ask(Request::ArchiveWorkspaces {
                project: project.clone(),
                workspaces: vec![a.id(), b.id(), a.id()],
                archived
            }),
            Response::Ack
        );
        assert_eq!(
            fixture.recorder.taken(),
            vec![DaemonEvent::WorkspacesChanged {
                project: project.clone()
            }]
        );
        let Response::Workspaces { workspaces } = fixture.ask(Request::ListWorkspaces {
            project: Some(project.clone()),
        }) else {
            panic!("expected workspaces")
        };
        for original in &before {
            let row = workspaces
                .iter()
                .find(|row| row.id() == original.id())
                .unwrap();
            let mut expected = original.worktree.clone();
            if [a.id(), b.id()].contains(&original.id()) {
                expected.archived = archived;
            }
            assert_eq!(row.worktree, expected);
        }
        for (path, (head, status)) in paths.iter().zip(&git_before) {
            assert_eq!(&support::git(path, &["rev-parse", "HEAD"]), head);
            assert_eq!(&support::git(path, &["status", "--porcelain"]), status);
        }
        assert_eq!(
            std::fs::read_to_string(paths[0].join("README.md")).unwrap(),
            "uncommitted tracked edit\n"
        );
        assert_eq!(
            std::fs::read_to_string(paths[1].join("archive-notes.txt")).unwrap(),
            "untracked notes\n"
        );
    }
}

#[test]
fn folder_catalog_reads_archived_metadata_without_git_or_push_events() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let original = new_workspace(&mut fixture, &project, "catalog").unwrap();
    let workspace = original.id();
    fixture.ask(Request::SetWorkspaceFolder {
        workspace: workspace.clone(),
        folder: Some("Review".into()),
    });
    fixture.ask(Request::PinWorkspace {
        workspace: workspace.clone(),
        pinned: true,
    });
    fixture.ask(Request::ArchiveWorkspace {
        workspace: workspace.clone(),
        archived: true,
    });
    fixture.recorder.taken();
    // A catalog is usable even when the registered worktree is temporarily absent.
    std::fs::remove_dir_all(&original.worktree.path).unwrap();
    assert_eq!(
        fixture.ask(Request::ListWorkspaceFolders {
            project: project.clone()
        }),
        Response::WorkspaceFolders {
            folders: vec![ginka_protocol::WorkspaceFolder {
                name: "Review".into(),
                active: 0,
                archived: 1
            }]
        }
    );
    assert!(fixture.recorder.taken().is_empty());
    fixture.ask(Request::SetWorkspaceFolder {
        workspace,
        folder: None,
    });
    fixture.recorder.taken();
    assert_eq!(
        fixture.ask(Request::ListWorkspaceFolders { project }),
        Response::WorkspaceFolders { folders: vec![] }
    );
    assert!(
        fixture
            .service
            .handle(Request::ListWorkspaceFolders {
                project: ProjectName("missing".into())
            })
            .is_err()
    );
    assert!(fixture.recorder.taken().is_empty());
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
fn external_editor_rejects_invalid_lines_and_paths_before_launch() {
    let mut fixture = Fixture::new();
    fixture.with_project();
    let workspace = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces[0].id(),
        other => panic!("expected workspaces, got {other:?}"),
    };
    for (path, line) in [
        ("README.md", Some(0)),
        ("missing.txt", Some(1)),
        ("../../missing.txt", None),
    ] {
        let result = fixture.service.handle(Request::OpenExternalEditor {
            workspace: workspace.clone(),
            path: path.into(),
            line,
        });
        assert!(result.is_err(), "{path} at {line:?} must be refused");
    }
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
fn provider_settings_apply_to_new_sessions_and_preserve_other_overrides() {
    let mut fixture = Fixture::configured(Service::with_settings_drivers);
    fixture.ask(Request::UpdateProviderSettings {
        provider: ginka_protocol::provider::ProviderKind::Codex,
        enabled: Some(false),
        program: Some("/opt/agents/codex".into()),
        clear_program: false,
    });
    let Response::ProviderSettings { providers } = fixture.ask(Request::ListProviderSettings)
    else {
        panic!("expected provider settings");
    };
    let codex = providers
        .iter()
        .find(|row| row.provider.as_str() == "codex")
        .unwrap();
    assert!(!codex.enabled);
    assert_eq!(codex.program.as_deref(), Some("/opt/agents/codex"));
    assert!(
        providers
            .iter()
            .find(|row| row.provider.as_str() == "claude")
            .unwrap()
            .enabled
    );
    let Response::Agents { agents } = fixture.ask(Request::ListAgents) else {
        panic!("expected agents");
    };
    assert!(!agents.iter().any(|agent| agent.id == "codex"));

    fixture.ask(Request::UpdateProviderSettings {
        provider: ginka_protocol::provider::ProviderKind::Codex,
        enabled: Some(true),
        program: None,
        clear_program: true,
    });
    let Response::ProviderSettings { providers } = fixture.ask(Request::ListProviderSettings)
    else {
        panic!("expected provider settings");
    };
    let codex = providers
        .iter()
        .find(|row| row.provider.as_str() == "codex")
        .unwrap();
    assert!(codex.enabled);
    assert_eq!(codex.program, None);

    fixture.ask(Request::UpdateProviderSettings {
        provider: ginka_protocol::provider::ProviderKind::Codex,
        enabled: None,
        program: Some("/opt/alternate/codex".into()),
        clear_program: false,
    });
    let Response::Agents { agents } = fixture.ask(Request::ListAgents) else {
        panic!("expected agents");
    };
    let codex = agents.iter().find(|agent| agent.id == "codex").unwrap();
    assert_eq!(codex.program, "/opt/alternate/codex");
    let Response::ProviderSettings { providers } = fixture.ask(Request::ListProviderSettings)
    else {
        panic!("expected provider settings");
    };
    let codex = providers
        .iter()
        .find(|row| row.provider.as_str() == "codex")
        .unwrap();
    assert!(codex.enabled);
    assert_eq!(codex.program.as_deref(), Some("/opt/alternate/codex"));
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
        context_lines: None,
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
        paths: Vec::new(),
    }) {
        Response::Committed { commit } => assert!(!commit.is_empty()),
        other => panic!("expected a commit, got {other:?}"),
    }
    let left = match fixture.ask(Request::WorkspaceChanges {
        workspace: id,
        source: ChangeSource::Uncommitted,
        context_lines: None,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert!(left.is_empty(), "nothing is left over: {left:?}");
}

#[test]
fn review_requests_keep_literal_paths_and_neighbouring_changes_separate() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "literal-review".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let worktree = &workspace.worktree.path;
    std::fs::write(worktree.join("[ab].txt"), "target\n").unwrap();
    std::fs::write(worktree.join("a.txt"), "neighbour\n").unwrap();
    let staged_paths = |fixture: &mut Fixture| match fixture.ask(Request::WorkspaceChanges {
        workspace: workspace.id(),
        source: ChangeSource::Staged,
        context_lines: None,
    }) {
        Response::Changes { changes } => changes
            .files
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>(),
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(
        fixture.ask(Request::StageFile {
            workspace: workspace.id(),
            path: "[ab].txt".into(),
            staged: true,
        }),
        Response::Ack
    );
    assert_eq!(staged_paths(&mut fixture), ["[ab].txt"]);
    fixture.ask(Request::StageFile {
        workspace: workspace.id(),
        path: "a.txt".into(),
        staged: true,
    });
    fixture.ask(Request::StageFile {
        workspace: workspace.id(),
        path: "[ab].txt".into(),
        staged: false,
    });
    assert_eq!(staged_paths(&mut fixture), ["a.txt"]);
    fixture.ask(Request::RevertFile {
        workspace: workspace.id(),
        path: "[ab].txt".into(),
    });
    assert!(!worktree.join("[ab].txt").exists());
    assert_eq!(
        std::fs::read_to_string(worktree.join("a.txt")).unwrap(),
        "neighbour\n"
    );
    assert_eq!(staged_paths(&mut fixture), ["a.txt"]);
}

#[test]
fn selected_commits_cross_the_service_without_consuming_other_staging() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "selected-commit".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let worktree = &workspace.worktree.path;
    std::fs::write(worktree.join("README.md"), "staged\n").unwrap();
    fixture.ask(Request::StageFile {
        workspace: workspace.id(),
        path: "README.md".into(),
        staged: true,
    });
    std::fs::write(worktree.join("README.md"), "staged and later\n").unwrap();
    std::fs::write(worktree.join("new.txt"), "selected\n").unwrap();
    assert!(matches!(
        fixture.ask(Request::Commit {
            workspace: workspace.id(),
            message: "Select new file".into(),
            all: true,
            amend: false,
            paths: vec!["new.txt".into()],
        }),
        Response::Committed { .. }
    ));
    let staged = match fixture.ask(Request::WorkspaceChanges {
        workspace: workspace.id(),
        source: ChangeSource::Staged,
        context_lines: None,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(
        staged
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["README.md"]
    );
    assert!(
        staged.files[0]
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| line.text == "staged")
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "staged and later\n"
    );
    for (all, amend) in [(false, false), (true, true)] {
        let error = fixture
            .service
            .handle(Request::Commit {
                workspace: workspace.id(),
                message: "Invalid selection".into(),
                all,
                amend,
                paths: vec!["README.md".into()],
            })
            .unwrap_err();
        assert!(error.message.contains("selected"), "{error}");
    }
    let error = fixture
        .service
        .handle(Request::GenerateCommitMessage {
            workspace: workspace.id(),
            agent: None,
            staged: true,
            paths: vec!["README.md".into()],
        })
        .unwrap_err();
    assert!(error.message.contains("selected"), "{error}");
}

#[test]
fn changes_context_crosses_the_service_boundary_and_is_bounded() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let baseline = (1..=30)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    std::fs::write(fixture.repo().join("README.md"), &baseline).unwrap();
    support::git(&fixture.repo(), &["add", "README.md"]);
    support::git(&fixture.repo(), &["commit", "-m", "Add context fixture"]);
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "context".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(
        workspace.worktree.path.join("README.md"),
        baseline.replace("line 15\n", "changed 15\n"),
    )
    .unwrap();
    let get = |fixture: &mut Fixture, context_lines| match fixture.ask(Request::WorkspaceChanges {
        workspace: workspace.id(),
        source: ChangeSource::Unstaged,
        context_lines,
    }) {
        Response::Changes { changes } => changes.files[0].hunks[0].lines.len(),
        other => panic!("expected changes, got {other:?}"),
    };
    assert_eq!(get(&mut fixture, None), 8);
    assert_eq!(get(&mut fixture, Some(10)), 22);
    assert!(
        fixture
            .service
            .handle(Request::WorkspaceChanges {
                workspace: workspace.id(),
                source: ChangeSource::Unstaged,
                context_lines: Some(26),
            })
            .is_err()
    );
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
        context_lines: None,
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
        context_lines: None,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    let left = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Unstaged,
        context_lines: None,
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
        context_lines: None,
    }) {
        Response::Changes { changes } => changes,
        other => panic!("expected changes, got {other:?}"),
    };
    let unstaged = match fixture.ask(Request::WorkspaceChanges {
        workspace: id.clone(),
        source: ChangeSource::Unstaged,
        context_lines: None,
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
        context_lines: None,
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
fn skill_requests_reach_names_beyond_the_previous_limit_and_toggle_all_copies() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".claude/skills");
    for n in 0..501 {
        let skill = root.join(format!("skill-{n:04}"));
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "Instructions").unwrap();
    }
    let mut fixture = Fixture::configured(|service| service.with_skills_home(home.path()));
    let project = fixture.with_project();
    let copy = fixture.repo().join(".agents/skills/skill-0500");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::write(copy.join("SKILL.md.disabled"), "Instructions").unwrap();
    match fixture.ask(Request::ListSkills {
        project: Some(project.clone()),
    }) {
        Response::Skills { skills, truncated } => {
            assert!(!truncated);
            assert_eq!(skills.len(), 501);
            let last = skills.last().unwrap();
            assert_eq!(last.name, "skill-0500");
            assert_eq!(last.installs.len(), 2);
            assert!(!last.enabled);
        }
        other => panic!("expected skills, got {other:?}"),
    }
    fixture.ask(Request::SetSkillEnabled {
        name: "skill-0500".into(),
        enabled: true,
        project: Some(project.clone()),
    });
    assert!(copy.join("SKILL.md").is_file());
    fixture.ask(Request::SetSkillEnabled {
        name: "skill-0500".into(),
        enabled: false,
        project: Some(project),
    });
    assert!(root.join("skill-0500/SKILL.md.disabled").is_file());
    assert!(copy.join("SKILL.md.disabled").is_file());
    assert!(root.join("skill-0499/SKILL.md").is_file());
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
fn creates_a_project_skill_and_rejects_a_duplicate() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let request = Request::CreateSkill {
        name: "review-guide".into(),
        description: "Review a proposed change".into(),
        body: "# Review\nCheck the tests.".into(),
        project: Some(project.clone()),
    };
    assert!(matches!(fixture.ask(request.clone()), Response::Ack));
    let path = fixture.repo().join(".agents/skills/review-guide/SKILL.md");
    assert!(path.is_file());
    let listed = fixture.ask(Request::ListSkills {
        project: Some(project),
    });
    let Response::Skills { skills, .. } = listed else {
        panic!("expected skills");
    };
    assert!(skills.iter().any(|skill| skill.name == "review-guide"));
    assert!(fixture.service.handle(request).is_err());
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .contains("Check the tests.")
    );
}

#[test]
fn creates_a_user_skill_without_a_registered_project() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::configured(|service| service.with_skills_home(home.path()));
    assert!(matches!(
        fixture.ask(Request::CreateSkill {
            name: "daily-review".into(),
            description: "Review daily changes".into(),
            body: "Check each changed file.".into(),
            project: None,
        }),
        Response::Ack
    ));
    assert!(
        home.path()
            .join(".agents/skills/daily-review/SKILL.md")
            .is_file()
    );
    let Response::Skills { skills, .. } = fixture.ask(Request::ListSkills { project: None }) else {
        panic!("expected skills");
    };
    assert!(skills.iter().any(|skill| skill.name == "daily-review"));
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
        context_lines: None,
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
                context_lines: None,
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
        session: None,
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
            session: None,
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
            session: None,
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
fn editing_a_scheduled_job_keeps_its_runs_and_rechecks_its_schedule() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let job = save_cron(&mut fixture, terminal_job(&project, "@yearly", "true"));
    fixture.ask(Request::RunCronJob { id: job.id });
    assert_eq!(cron_runs(&mut fixture, job.id).len(), 1);

    let at = chrono::Utc::now() + chrono::Duration::minutes(3);
    let schedule = format!("@once {}", at.to_rfc3339());
    let edited = save_cron(
        &mut fixture,
        Request::SaveCronJob {
            id: Some(job.id),
            project,
            workspace: None,
            session: None,
            name: "follow up".into(),
            schedule: schedule.clone(),
            via: ginka_protocol::model::CronVia::Terminal,
            agent: None,
            body: "echo ready".into(),
            precheck: None,
            enabled: true,
        },
    );
    assert_eq!(edited.id, job.id);
    assert_eq!(edited.name, "follow up");
    assert_eq!(edited.schedule, schedule);
    assert_eq!(edited.next_run_at, Some(at.timestamp()));
    assert_eq!(cron_runs(&mut fixture, job.id).len(), 1);
}

#[test]
fn a_one_time_job_requires_a_future_zoned_timestamp_and_fires_only_once() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let past = chrono::Utc::now() - chrono::Duration::minutes(1);
    for schedule in [
        "@once".to_string(),
        "@once tomorrow".to_string(),
        "@once 2026-10-01T09:00:00".to_string(),
        format!("@once {}", past.to_rfc3339()),
    ] {
        assert!(
            fixture
                .service
                .handle(terminal_job(&project, &schedule, "true"))
                .is_err(),
            "{schedule:?} was accepted"
        );
    }

    let at = chrono::Utc::now() + chrono::Duration::minutes(2);
    let schedule = format!("@once {}", at.to_rfc3339());
    let job = save_cron(&mut fixture, terminal_job(&project, &schedule, "true"));
    assert_eq!(job.next_run_at, Some(at.timestamp()));
    assert_eq!(fixture.service.run_due_cron(chrono::Local::now()), 0);

    let due = at.with_timezone(&chrono::Local) + chrono::Duration::seconds(1);
    assert_eq!(fixture.service.run_due_cron(due), 1);
    let claimed = match fixture.ask(Request::ListCronJobs { project: None }) {
        Response::CronJobs { jobs } => jobs.into_iter().next().unwrap(),
        other => panic!("expected jobs, got {other:?}"),
    };
    assert!(!claimed.enabled, "claiming a one-time job disables it");
    assert_eq!(claimed.next_run_at, None);
    assert_eq!(
        fixture
            .service
            .run_due_cron(due + chrono::Duration::days(1)),
        0
    );
    assert_eq!(cron_runs(&mut fixture, job.id).len(), 1);
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

#[test]
fn a_branch_diff_reads_everything_since_it_left_its_base() {
    // Orca's "compare against the base branch": what the whole branch did,
    // committed or not, measured from where it forked.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "feature".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let path = workspace.worktree.path.clone();
    let git = |dir: &std::path::Path, args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success(),
            "git {args:?}"
        );
    };
    std::fs::write(path.join("committed.txt"), "one\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "on the branch"]);
    std::fs::write(path.join("pending.txt"), "two\n").unwrap();
    // The base moves on after the fork; that is not the branch's work.
    std::fs::write(fixture.repo().join("upstream.txt"), "later\n").unwrap();
    git(&fixture.repo(), &["add", "-A"]);
    git(&fixture.repo(), &["commit", "-q", "-m", "base moved"]);

    for base in [None, Some("main".to_string())] {
        let changes = match fixture.ask(Request::WorkspaceChanges {
            workspace: workspace.id(),
            source: ChangeSource::Branch { base: base.clone() },
            context_lines: None,
        }) {
            Response::Changes { changes } => changes,
            other => panic!("expected changes, got {other:?}"),
        };
        let mut paths: Vec<_> = changes
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        paths.sort();
        assert_eq!(paths, ["committed.txt", "pending.txt"], "base {base:?}");
    }

    for hostile in ["--output=/tmp/ginka-pwned", "main..HEAD", "no-such-branch"] {
        assert!(
            fixture
                .service
                .handle(Request::WorkspaceChanges {
                    workspace: workspace.id(),
                    source: ChangeSource::Branch {
                        base: Some(hostile.into()),
                    },
                    context_lines: None,
                })
                .is_err(),
            "{hostile} is refused"
        );
    }
}

/// A git hook that says it started, then holds git until the test lets it
/// go: a slow remote or a slow lint, without either. Returns the "started"
/// and "release" files.
fn held_hook(fixture: &Fixture, hook: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let started = fixture.work.path().join(format!("{hook}.started"));
    let release = fixture.work.path().join(format!("{hook}.release"));
    let hooks = fixture.work.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join(hook);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.05; done\n",
            started.display(),
            release.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    support::git(
        &fixture.repo(),
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    (started, release)
}

/// Run `request` through the shared service on another thread while its
/// hook holds it, and say whether another request was answered meanwhile.
fn served_while_held(
    service: Service,
    request: Request,
    (started, release): (std::path::PathBuf, std::path::PathBuf),
) -> (bool, Result<Response, ginka_protocol::RpcError>) {
    let service = Arc::new(Mutex::new(service));
    let running = {
        let service = service.clone();
        std::thread::spawn(move || ginka_core::service::handle_shared(&service, request))
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !started.exists() {
        assert!(std::time::Instant::now() < deadline, "the hook never ran");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut served = false;
    while std::time::Instant::now() < deadline {
        if let Ok(mut service) = service.try_lock() {
            served = matches!(
                service.handle(Request::ListProjects),
                Ok(Response::Projects { .. })
            );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::write(&release, "").unwrap();
    (served, running.join().unwrap())
}

#[test]
fn a_push_leaves_the_service_free_for_other_requests_while_it_waits_on_the_remote() {
    // The daemon serves every client through one `Service`. A push, a pull
    // or a `gh` call can wait on the network for as long as the network
    // likes; holding the service through that would freeze every window.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let remote = fixture.work.path().join("remote.git");
    support::git(fixture.work.path(), &["init", "--bare", "-q", "remote.git"]);
    support::git(
        &fixture.repo(),
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "slow-push".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace.id(),
        other => panic!("expected a workspace, got {other:?}"),
    };
    let hook = held_hook(&fixture, "pre-push");

    let (served, pushed) = served_while_held(
        fixture.service,
        Request::Push {
            workspace,
            force_with_lease: false,
        },
        hook,
    );

    assert!(served, "the service stayed locked while the push waited");
    assert!(matches!(pushed, Ok(Response::Ack)), "{pushed:?}");
    assert!(
        support::git(&remote, &["branch", "--list", "slow-push"]).contains("slow-push"),
        "the push reached the remote"
    );
    assert!(
        fixture
            .recorder
            .taken()
            .contains(&DaemonEvent::WorkspacesChanged { project })
    );
}

#[test]
fn a_commit_leaves_the_service_free_while_its_hooks_run() {
    // A pre-commit hook is the project's code: a lint that takes a minute,
    // or one that itself asks `ginka` something — which, with the service
    // held, would wait on the request it is part of forever.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "slow-hook".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(workspace.worktree.path.join("new.txt"), "new\n").unwrap();
    let hook = held_hook(&fixture, "pre-commit");

    let (served, committed) = served_while_held(
        fixture.service,
        Request::Commit {
            workspace: workspace.id(),
            message: "add new".into(),
            all: true,
            amend: false,
            paths: Vec::new(),
        },
        hook,
    );

    assert!(served, "the service stayed locked while the hook ran");
    assert!(
        matches!(committed, Ok(Response::Committed { .. })),
        "{committed:?}"
    );
    assert!(
        fixture
            .recorder
            .taken()
            .contains(&DaemonEvent::WorkspacesChanged { project })
    );
}

#[test]
fn a_merge_leaves_the_service_free_while_its_hooks_run() {
    // Merging commits uncommitted work first and then merges in the
    // project's checkout: both run the project's hooks.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "slow-merge".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(workspace.worktree.path.join("merged.txt"), "merged\n").unwrap();
    let hook = held_hook(&fixture, "pre-commit");
    let checkout = fixture.repo();

    let (served, merged) = served_while_held(
        fixture.service,
        Request::MergeWorkspace {
            workspace: workspace.id(),
            into: None,
            message: Some("add merged".into()),
        },
        hook,
    );

    assert!(served, "the service stayed locked while the hook ran");
    assert!(matches!(merged, Ok(Response::Merged { .. })), "{merged:?}");
    assert!(checkout.join("merged.txt").is_file());
    assert!(
        fixture
            .recorder
            .taken()
            .contains(&DaemonEvent::WorkspacesChanged { project })
    );
}

#[test]
fn listing_workspaces_leaves_the_service_free_while_git_reads_them() {
    // Every window lists workspaces on its polling interval, and each one is
    // a `git status`; on a large repository that is long enough to hold up
    // every other request. A filesystem monitor that waits stands in for a
    // slow status.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "slow-status".into(),
        base: None,
    });
    let (started, release) = held_hook(&fixture, "fsmonitor");
    support::git(&fixture.repo(), &["config", "--unset", "core.hooksPath"]);
    let monitor = fixture.work.path().join("hooks").join("fsmonitor");
    support::git(
        &fixture.repo(),
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );

    let (served, listed) = served_while_held(
        fixture.service,
        Request::ListWorkspaces {
            project: Some(project),
        },
        (started, release),
    );

    assert!(
        served,
        "the service stayed locked while git read the worktrees"
    );
    match listed {
        Ok(Response::Workspaces { workspaces }) => {
            assert!(
                workspaces
                    .iter()
                    .any(|workspace| workspace.worktree.branch == "slow-status")
            )
        }
        other => panic!("expected workspaces, got {other:?}"),
    }
}

#[test]
fn reading_a_diff_leaves_the_service_free_while_git_works() {
    // The Git surface reads the worktree's diff on its polling interval,
    // twice (staged and unstaged), from every window that shows it.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "slow-diff".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    std::fs::write(workspace.worktree.path.join("README.md"), "changed\n").unwrap();
    let (started, release) = held_hook(&fixture, "fsmonitor");
    support::git(&fixture.repo(), &["config", "--unset", "core.hooksPath"]);
    let monitor = fixture.work.path().join("hooks").join("fsmonitor");
    support::git(
        &fixture.repo(),
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );

    let (served, read) = served_while_held(
        fixture.service,
        Request::WorkspaceChanges {
            workspace: workspace.id(),
            source: ChangeSource::Unstaged,
            context_lines: None,
        },
        (started, release),
    );

    assert!(served, "the service stayed locked while git read the diff");
    match read {
        Ok(Response::Changes { changes }) => {
            assert!(changes.files.iter().any(|file| file.path == "README.md"))
        }
        other => panic!("expected changes, got {other:?}"),
    }
}

#[test]
fn creating_a_workspace_leaves_the_service_free_while_git_checks_it_out() {
    // A checkout of a large repository, its post-checkout hook and the
    // files `.worktreeinclude` copies all take as long as they take.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let hook = held_hook(&fixture, "post-checkout");

    let (served, created) = served_while_held(
        fixture.service,
        Request::CreateWorkspace {
            project: project.clone(),
            branch: "slow-checkout".into(),
            base: None,
        },
        hook,
    );

    assert!(served, "the service stayed locked while git checked out");
    match created {
        Ok(Response::Workspace { workspace }) => {
            assert_eq!(workspace.worktree.branch, "slow-checkout");
            assert!(workspace.worktree.path.join("README.md").is_file());
        }
        other => panic!("expected a workspace, got {other:?}"),
    }
    assert!(
        fixture
            .recorder
            .taken()
            .contains(&DaemonEvent::WorkspacesChanged { project })
    );
}

#[test]
fn polling_statuses_leaves_the_service_free_while_git_reads_them() {
    // The daemon's own tick reads every worktree's status; requests must
    // not wait for it.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    fixture.ask(Request::CreateWorkspace {
        project,
        branch: "slow-poll".into(),
        base: None,
    });
    let (started, release) = held_hook(&fixture, "fsmonitor");
    support::git(&fixture.repo(), &["config", "--unset", "core.hooksPath"]);
    let monitor = fixture.work.path().join("hooks").join("fsmonitor");
    support::git(
        &fixture.repo(),
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );
    let recorder = fixture.recorder.clone();
    recorder.taken();

    let service = Arc::new(Mutex::new(fixture.service));
    let polling = {
        let service = service.clone();
        std::thread::spawn(move || ginka_core::service::poll_statuses_shared(&service))
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !started.exists() {
        assert!(std::time::Instant::now() < deadline, "git never ran");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut served = false;
    while std::time::Instant::now() < deadline {
        if let Ok(mut service) = service.try_lock() {
            served = service.handle(Request::ListProjects).is_ok();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::write(&release, "").unwrap();
    polling.join().unwrap();

    assert!(
        served,
        "the service stayed locked while the poller read git"
    );
    assert!(
        recorder
            .taken()
            .iter()
            .any(|event| matches!(event, DaemonEvent::WorkspaceStatusChanged { .. })),
        "what the poller read is still announced"
    );
}

#[test]
fn switching_branches_leaves_the_service_free_while_the_hook_runs() {
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project,
        branch: "slow-switch".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace.id(),
        other => panic!("expected a workspace, got {other:?}"),
    };
    let hook = held_hook(&fixture, "post-checkout");

    let (served, switched) = served_while_held(
        fixture.service,
        Request::CheckoutBranch {
            workspace,
            branch: "elsewhere".into(),
            create: true,
        },
        hook,
    );

    assert!(served, "the service stayed locked while the hook ran");
    assert!(matches!(switched, Ok(Response::Ack)), "{switched:?}");
}

#[test]
fn removing_a_worktree_leaves_the_service_free_and_the_workspace_closed_meanwhile() {
    // Deleting a large worktree takes as long as the disk does. Other
    // requests are answered meanwhile, and the workspace being removed is
    // refused rather than handed to an agent halfway through its deletion.
    let mut fixture = Fixture::new();
    let project = fixture.with_project();
    let workspace = match fixture.ask(Request::CreateWorkspace {
        project: project.clone(),
        branch: "slow-removal".into(),
        base: None,
    }) {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let (started, release) = held_hook(&fixture, "fsmonitor");
    support::git(&fixture.repo(), &["config", "--unset", "core.hooksPath"]);
    let monitor = fixture.work.path().join("hooks").join("fsmonitor");
    support::git(
        &fixture.repo(),
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );

    let service = Arc::new(Mutex::new(fixture.service));
    let removing = {
        let service = service.clone();
        let workspace = workspace.id();
        std::thread::spawn(move || {
            ginka_core::service::handle_shared(
                &service,
                Request::RemoveWorkspace {
                    workspace,
                    force: false,
                },
            )
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !started.exists() {
        assert!(std::time::Instant::now() < deadline, "git never ran");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut served = None;
    while std::time::Instant::now() < deadline {
        if let Ok(mut service) = service.try_lock() {
            let other = service.handle(Request::ListProjects).is_ok();
            let same = service.handle(Request::WorkspaceHistory {
                workspace: workspace.id(),
                limit: None,
            });
            served = Some((other, same));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::write(&release, "").unwrap();
    let removed = removing.join().unwrap();

    let (other, same) = served.expect("the service stayed locked while git removed the worktree");
    assert!(other);
    let error = same.expect_err("a workspace being removed is not worked on");
    assert!(error.message.contains("being removed"), "{}", error.message);
    assert!(matches!(removed, Ok(Response::Ack)), "{removed:?}");
    assert!(!workspace.worktree.path.exists());
}
