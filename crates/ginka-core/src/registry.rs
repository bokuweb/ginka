//! Registering projects and keeping worktrees in step with git.
//!
//! Git is the authority on which worktrees exist; the database is a cache of
//! it plus the things git does not know (the immutable workspace name, whether
//! a workspace is pinned). Reconciliation therefore always runs git-first, and
//! is written to be safe to run on a tick.

use crate::git;
use crate::project::{Project, ProjectKind, Worktree, insert_project, list_worktrees};
use anyhow::{Context, Result};
use ginka_protocol::{ProjectName, ids::slugify};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Register a folder as a project, probing git for what it can tell us.
///
/// A folder that is not a repository is registered as `plain`: one implicit
/// workspace, no branches, git features off. That is a supported way to work,
/// not an error — plenty of useful agent tasks happen outside a repository.
pub fn register_project(conn: &Connection, path: &Path) -> Result<Project> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;

    let (kind, root) = if git::is_repository(&path) {
        (ProjectKind::Git, git::top_level(&path)?)
    } else {
        (ProjectKind::Plain, path)
    };

    let name = ProjectName(project_name_for(&root));
    let project = Project {
        default_branch: match kind {
            ProjectKind::Git => git::default_branch(&root),
            ProjectKind::Plain => String::new(),
        },
        has_origin: match kind {
            ProjectKind::Git => Some(git::has_origin(&root)),
            ProjectKind::Plain => Some(false),
        },
        name,
        path: root,
        label: None,
        sort_order: next_sort_order(conn)?,
        kind,
    };

    insert_project(conn, &project)?;
    Ok(project)
}

/// Derive a project's key from its directory name.
///
/// Slugified so it is safe in paths, ids and shell arguments. A directory whose
/// name slugifies to nothing — one written entirely in a non-ASCII script —
/// falls back to a fixed word rather than producing an empty primary key.
fn project_name_for(root: &Path) -> String {
    let raw = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = slugify(&raw);
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}

fn next_sort_order(conn: &Connection) -> Result<i64> {
    let highest: Option<i64> =
        conn.query_row("SELECT max(sort_order) FROM projects", [], |row| row.get(0))?;
    Ok(highest.map(|value| value + 1).unwrap_or(0))
}

/// What a reconciliation changed, for logging and for tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

impl SyncReport {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Reconcile the stored worktrees for `project` against git.
///
/// Adopts worktrees created outside the app, updates the live branch and head
/// of ones we already knew, and forgets ones git no longer has. The immutable
/// `name` is assigned once, on adoption, and never rewritten — an agent that
/// switches branches inside a worktree must not re-key the workspace.
pub fn sync_worktrees(conn: &Connection, project: &Project) -> Result<SyncReport> {
    if project.kind == ProjectKind::Plain {
        return Ok(SyncReport::default());
    }

    let live = git::list_worktrees(&project.path)?;
    let known = list_worktrees(conn, &project.name)?;
    let mut report = SyncReport::default();

    for worktree in &live {
        // A worktree git has forgotten the directory for is not a workspace.
        if worktree.prunable {
            continue;
        }
        let branch = worktree.branch_label();
        let existing = known
            .iter()
            .find(|candidate| candidate.path == worktree.path);

        match existing {
            Some(existing) => {
                if existing.branch != branch || existing.head.as_deref() != worktree.head.as_deref()
                {
                    conn.execute(
                        "UPDATE worktrees SET branch = ?1, head = ?2 \
                         WHERE project_name = ?3 AND path = ?4",
                        rusqlite::params![
                            branch,
                            worktree.head,
                            project.name.0,
                            worktree.path.to_string_lossy(),
                        ],
                    )?;
                    report.updated += 1;
                }
            }
            None => {
                conn.execute(
                    "INSERT INTO worktrees (project_name, name, branch, path, head) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        project.name.0,
                        unique_name(&branch, &known),
                        branch,
                        worktree.path.to_string_lossy(),
                        worktree.head,
                    ],
                )?;
                report.added += 1;
            }
        }
    }

    let live_paths: Vec<&PathBuf> = live
        .iter()
        .filter(|worktree| !worktree.prunable)
        .map(|worktree| &worktree.path)
        .collect();
    for worktree in &known {
        if !live_paths.contains(&&worktree.path) {
            conn.execute(
                "DELETE FROM worktrees WHERE project_name = ?1 AND path = ?2",
                rusqlite::params![project.name.0, worktree.path.to_string_lossy()],
            )?;
            report.removed += 1;
        }
    }

    Ok(report)
}

/// The immutable workspace name, made unique within the project.
///
/// Two branches can slugify to the same string (`fix/A` and `fix-a`), and the
/// name is a unique key, so collisions get a numeric suffix rather than an
/// insert failure.
fn unique_name(branch: &str, known: &[Worktree]) -> String {
    let base = {
        let slug = slugify(branch);
        if slug.is_empty() {
            "workspace".to_string()
        } else {
            slug
        }
    };
    if !known.iter().any(|worktree| worktree.name == base) {
        return base;
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !known.iter().any(|worktree| &worktree.name == candidate))
        .expect("the range is unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::project::list_projects;
    use std::process::Command;

    /// A repository with one commit on `main`.
    fn repository(root: &Path) {
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .expect("git is available");
            assert!(status.status.success(), "git {args:?}");
        };
        std::fs::create_dir_all(root).unwrap();
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), "hello").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "first"]);
    }

    #[test]
    fn a_repository_registers_as_a_git_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("comet");
        repository(&root);

        let conn = db::open_in_memory().unwrap();
        let project = register_project(&conn, &root).unwrap();

        assert_eq!(project.name.0, "comet");
        assert_eq!(project.kind, ProjectKind::Git);
        assert_eq!(project.default_branch, "main");
        // No remote was added, so CI and PR features have nothing to query.
        assert_eq!(project.has_origin, Some(false));
        assert_eq!(list_projects(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_plain_folder_registers_without_git_features() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("notes");
        std::fs::create_dir_all(&root).unwrap();

        let conn = db::open_in_memory().unwrap();
        let project = register_project(&conn, &root).unwrap();
        assert_eq!(project.kind, ProjectKind::Plain);
        assert!(project.default_branch.is_empty());
    }

    #[test]
    fn registering_a_subdirectory_records_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("comet");
        repository(&root);
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let conn = db::open_in_memory().unwrap();
        let project = register_project(&conn, &nested).unwrap();
        assert_eq!(project.name.0, "comet");
        assert_eq!(project.path, root.canonicalize().unwrap());
    }

    #[test]
    fn sync_adopts_the_main_worktree_then_settles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("comet");
        repository(&root);

        let conn = db::open_in_memory().unwrap();
        let project = register_project(&conn, &root).unwrap();

        let first = sync_worktrees(&conn, &project).unwrap();
        assert_eq!(first.added, 1);

        // Running again on an unchanged repository must be a no-op, because it
        // runs on a tick.
        let second = sync_worktrees(&conn, &project).unwrap();
        assert!(second.is_empty(), "{second:?}");
    }

    #[test]
    fn sync_adopts_a_worktree_created_outside_the_app_and_forgets_a_removed_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("comet");
        repository(&root);
        let conn = db::open_in_memory().unwrap();
        let project = register_project(&conn, &root).unwrap();
        sync_worktrees(&conn, &project).unwrap();

        let extra = dir.path().join("bright-harbor");
        git::add_worktree(&project.path, &extra, "bright-harbor", "main").unwrap();

        let report = sync_worktrees(&conn, &project).unwrap();
        assert_eq!(report.added, 1);
        let stored = list_worktrees(&conn, &project.name).unwrap();
        assert!(stored.iter().any(|w| w.name == "bright-harbor"));

        git::remove_worktree(&project.path, &extra, false).unwrap();
        let report = sync_worktrees(&conn, &project).unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(list_worktrees(&conn, &project.name).unwrap().len(), 1);
    }

    #[test]
    fn switching_a_branch_updates_it_without_renaming_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("comet");
        repository(&root);
        let conn = db::open_in_memory().unwrap();
        let project = register_project(&conn, &root).unwrap();
        sync_worktrees(&conn, &project).unwrap();

        let extra = dir.path().join("bright-harbor");
        git::add_worktree(&project.path, &extra, "bright-harbor", "main").unwrap();
        sync_worktrees(&conn, &project).unwrap();

        let before = list_worktrees(&conn, &project.name)
            .unwrap()
            .into_iter()
            .find(|w| w.path == extra.canonicalize().unwrap() || w.path == extra)
            .expect("the worktree was adopted");

        // An agent switches branches inside the worktree.
        Command::new("git")
            .arg("-C")
            .arg(&extra)
            .args(["checkout", "-b", "some/other-branch"])
            .output()
            .unwrap();

        let report = sync_worktrees(&conn, &project).unwrap();
        assert_eq!(report.updated, 1);

        let after = list_worktrees(&conn, &project.name)
            .unwrap()
            .into_iter()
            .find(|w| w.path == before.path)
            .unwrap();
        assert_eq!(
            after.branch, "some/other-branch",
            "the live branch tracks git"
        );
        assert_eq!(
            after.name, before.name,
            "the workspace identity must not move"
        );
        assert_eq!(after.workspace_id(), before.workspace_id());
    }

    #[test]
    fn colliding_slugs_get_distinct_names() {
        let known = vec![Worktree {
            project: ProjectName("p".into()),
            name: "fix-a".into(),
            branch: "fix/a".into(),
            path: PathBuf::from("/tmp/one"),
            head: None,
            pinned: false,
        }];
        // `fix-a` and `fix/a` slugify identically; the second must not collide
        // with the first on a unique key.
        assert_eq!(unique_name("fix-a", &known), "fix-a-2");
        assert_eq!(unique_name("fix/b", &known), "fix-b");
    }

    #[test]
    fn a_name_that_slugifies_to_nothing_still_produces_a_key() {
        assert_eq!(unique_name("日本語", &[]), "workspace");
        assert_eq!(project_name_for(Path::new("/tmp/日本語")), "project");
    }
}
