//! Persistence for projects and worktrees.
//!
//! The rows themselves are `ginka-protocol` types: the CLI and the app read
//! them straight off the wire, so there is one definition of what a project is
//! rather than a domain copy and a wire copy that drift.

use anyhow::Result;
use ginka_protocol::model::StatusNote;
use ginka_protocol::{ProjectName, WorkspaceId};
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;

pub use ginka_protocol::model::{Project, ProjectKind, Worktree};

/// Insert a project, or update the one already stored under its name.
///
/// Upsert rather than insert because registering an existing project is how a
/// user tells Ginka the repository moved on disk.
pub fn insert_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "INSERT INTO projects (name, path, default_branch, label, sort_order, kind, has_origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(name) DO UPDATE SET
             path = excluded.path,
             default_branch = excluded.default_branch,
             label = excluded.label,
             sort_order = excluded.sort_order,
             kind = excluded.kind,
             has_origin = excluded.has_origin",
        rusqlite::params![
            project.name.0,
            project.path.to_string_lossy(),
            project.default_branch,
            project.label,
            project.sort_order,
            project.kind.as_str(),
            project.has_origin,
        ],
    )?;
    Ok(())
}

/// Set or clear a project's label. `false` when there is no such project.
pub fn set_label(conn: &Connection, project: &ProjectName, label: Option<&str>) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE projects SET label = ?1 WHERE name = ?2",
        rusqlite::params![label, project.0],
    )? > 0)
}

/// Put a project at `index` in the sidebar order, renumbering every project
/// so the order is dense. `false` when there is no such project.
pub fn move_to(conn: &Connection, project: &ProjectName, index: usize) -> Result<bool> {
    let mut names: Vec<ProjectName> = list_projects(conn)?
        .into_iter()
        .map(|project| project.name)
        .collect();
    let Some(from) = names.iter().position(|name| name == project) else {
        return Ok(false);
    };
    let moved = names.remove(from);
    names.insert(index.min(names.len()), moved);
    let transaction = conn.unchecked_transaction()?;
    for (order, name) in names.iter().enumerate() {
        transaction.execute(
            "UPDATE projects SET sort_order = ?1 WHERE name = ?2",
            rusqlite::params![order as i64, name.0],
        )?;
    }
    transaction.commit()?;
    Ok(true)
}

/// Every project, in sidebar order.
pub fn list_projects(conn: &Connection) -> Result<Vec<Project>> {
    let mut statement = conn.prepare(
        "SELECT name, path, default_branch, label, sort_order, kind, has_origin
         FROM projects ORDER BY sort_order, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Project {
            name: ProjectName(row.get(0)?),
            path: PathBuf::from(row.get::<_, String>(1)?),
            default_branch: row.get(2)?,
            label: row.get(3)?,
            sort_order: row.get(4)?,
            kind: ProjectKind::parse(&row.get::<_, String>(5)?),
            has_origin: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Every worktree stored for `project`, pinned ones first.
pub fn list_worktrees(conn: &Connection, project: &ProjectName) -> Result<Vec<Worktree>> {
    let mut statement = conn.prepare(
        "SELECT project_name, name, branch, path, head, pinned, archived, folder
         FROM worktrees
         WHERE project_name = ?1
         ORDER BY archived, pinned DESC, name",
    )?;
    let rows = statement.query_map([&project.0], |row| {
        Ok(Worktree {
            project: ProjectName(row.get(0)?),
            name: row.get(1)?,
            branch: row.get(2)?,
            path: PathBuf::from(row.get::<_, String>(3)?),
            head: row.get(4)?,
            pinned: row.get(5)?,
            archived: row.get(6)?,
            folder: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Forget a project. Cascades to its worktrees; leaves the user's code alone.
pub fn remove_project(conn: &Connection, project: &ProjectName) -> Result<bool> {
    let removed = conn.execute("DELETE FROM projects WHERE name = ?1", [&project.0])?;
    Ok(removed > 0)
}

/// The worktree a workspace id names, if it is still stored.
///
/// The id carries the project and the immutable worktree name, which is why
/// this lookup keeps working after an agent switches branches inside it.
pub fn find_worktree(conn: &Connection, workspace: &WorkspaceId) -> Result<Option<Worktree>> {
    let Some((project, name)) = workspace.parts() else {
        return Ok(None);
    };
    Ok(list_worktrees(conn, &project)?
        .into_iter()
        .find(|worktree| worktree.name == name))
}

/// Pin or unpin a workspace. Returns whether a row was affected.
///
/// Pinning is ours rather than git's, so it lives only here and reconciliation
/// must never write over it.
pub fn set_pinned(conn: &Connection, workspace: &WorkspaceId, pinned: bool) -> Result<bool> {
    let Some((project, name)) = workspace.parts() else {
        return Ok(false);
    };
    let updated = conn.execute(
        "UPDATE worktrees SET pinned = ?1 WHERE project_name = ?2 AND name = ?3",
        rusqlite::params![pinned, project.0, name],
    )?;
    Ok(updated > 0)
}

/// Summarize assigned folder labels for one project, including archived workspaces.
pub fn list_workspace_folders(
    conn: &Connection,
    project: &ProjectName,
) -> Result<Vec<ginka_protocol::WorkspaceFolder>> {
    let mut stmt = conn.prepare(
        "SELECT folder, SUM(NOT archived), SUM(archived) FROM worktrees
         WHERE project_name = ?1 AND folder IS NOT NULL GROUP BY folder ORDER BY folder",
    )?;
    let folders = stmt.query_map([&project.0], |row| {
        Ok(ginka_protocol::WorkspaceFolder {
            name: row.get(0)?,
            active: row.get(1)?,
            archived: row.get(2)?,
        })
    })?;
    Ok(folders.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Assign a project-local sidebar folder, or clear it with a blank or `None`.
///
/// Names are trimmed and limited to 80 characters without control characters.
/// Invalid names leave the previous value intact. Returns whether the immutable
/// workspace id matched a stored row; git reconciliation does not own this value.
pub fn set_folder(
    conn: &Connection,
    workspace: &WorkspaceId,
    folder: Option<&str>,
) -> Result<bool> {
    let folder = folder.map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = folder {
        anyhow::ensure!(
            !name.chars().any(char::is_control) && name.chars().count() <= 80,
            "folder name must be at most 80 characters without control characters"
        );
    }
    let Some((project, name)) = workspace.parts() else {
        return Ok(false);
    };
    Ok(conn.execute(
        "UPDATE worktrees SET folder = ?1 WHERE project_name = ?2 AND name = ?3",
        rusqlite::params![folder, project.0, name],
    )? > 0)
}

/// Archive or restore a workspace. Returns whether a row was affected.
///
/// Archive state is app-owned metadata, like pinning. Git reconciliation must
/// preserve it: archiving removes clutter, not the worktree or its history.
pub fn set_archived(conn: &Connection, workspace: &WorkspaceId, archived: bool) -> Result<bool> {
    let Some((project, name)) = workspace.parts() else {
        return Ok(false);
    };
    let updated = conn.execute(
        "UPDATE worktrees SET archived = ?1 WHERE project_name = ?2 AND name = ?3",
        rusqlite::params![archived, project.0, name],
    )?;
    Ok(updated > 0)
}

/// Write, or with `None` clear, a workspace's status note. Returns whether a
/// row was affected.
///
/// `text` is stored as [`StatusNote::normalize`] leaves it, so a blank note
/// clears rather than showing an empty line.
pub fn set_status_note(
    conn: &Connection,
    workspace: &WorkspaceId,
    text: Option<&str>,
    now: i64,
) -> Result<bool> {
    let Some((project, name)) = workspace.parts() else {
        return Ok(false);
    };
    let text = text.and_then(StatusNote::normalize);
    let at = text.as_ref().map(|_| now);
    let updated = conn.execute(
        "UPDATE worktrees SET status_note = ?1, status_note_at = ?2
         WHERE project_name = ?3 AND name = ?4",
        rusqlite::params![text, at, project.0, name],
    )?;
    Ok(updated > 0)
}

/// The status note on a workspace, if one is written.
pub fn status_note(conn: &Connection, workspace: &WorkspaceId) -> Result<Option<StatusNote>> {
    let Some((project, name)) = workspace.parts() else {
        return Ok(None);
    };
    let row = conn
        .query_row(
            "SELECT status_note, status_note_at FROM worktrees
             WHERE project_name = ?1 AND name = ?2",
            rusqlite::params![project.0, name],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        Some((Some(text), set_at)) => Some(StatusNote {
            text,
            set_at: set_at.unwrap_or_default(),
        }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn sample() -> Project {
        Project {
            name: ProjectName("comet".into()),
            path: PathBuf::from("/tmp/comet"),
            default_branch: "main".into(),
            label: Some("personal-metal".into()),
            sort_order: 0,
            kind: ProjectKind::Git,
            has_origin: None,
        }
    }

    #[test]
    fn folder_catalog_is_project_local_counts_archived_and_drops_empty_folders() {
        let conn = db::open_in_memory().unwrap();
        insert_project(&conn, &sample()).unwrap();
        let mut other = sample();
        other.name = ProjectName("other".into());
        insert_project(&conn, &other).unwrap();
        for (project, name, folder, archived, pinned) in [
            ("comet", "a", Some("Review"), false, true),
            ("comet", "b", Some("Review"), true, false),
            ("comet", "c", Some("Archive only"), true, false),
            ("comet", "d", None, false, false),
            ("other", "e", Some("Foreign"), false, false),
        ] {
            conn.execute(
                "INSERT INTO worktrees (project_name, name, branch, path, folder, archived, pinned) VALUES (?1, ?2, 'main', ?2, ?3, ?4, ?5)",
                rusqlite::params![project, name, folder, archived, pinned],
            ).unwrap();
        }
        let folders = list_workspace_folders(&conn, &sample().name).unwrap();
        assert_eq!(
            folders
                .iter()
                .map(|f| (f.name.as_str(), f.active, f.archived))
                .collect::<Vec<_>>(),
            vec![("Archive only", 0, 1), ("Review", 1, 1)]
        );
        set_folder(&conn, &WorkspaceId("comet/c".into()), None).unwrap();
        assert_eq!(
            list_workspace_folders(&conn, &sample().name).unwrap().len(),
            1
        );
        assert!(
            list_workspace_folders(&conn, &ProjectName("missing".into()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn folders_are_trimmed_cleared_and_invalid_names_leave_the_value_intact() {
        let conn = db::open_in_memory().unwrap();
        insert_project(&conn, &sample()).unwrap();
        conn.execute("INSERT INTO worktrees (project_name, name, branch, path) VALUES ('comet', 'work', 'main', '/tmp/work')", []).unwrap();
        let id = WorkspaceId("comet/work".into());
        assert!(set_folder(&conn, &id, Some("  Review  ")).unwrap());
        assert_eq!(
            find_worktree(&conn, &id)
                .unwrap()
                .unwrap()
                .folder
                .as_deref(),
            Some("Review")
        );
        for invalid in ["one\ntwo".to_string(), "a\tb".to_string(), "x".repeat(81)] {
            assert!(set_folder(&conn, &id, Some(&invalid)).is_err());
            assert_eq!(
                find_worktree(&conn, &id)
                    .unwrap()
                    .unwrap()
                    .folder
                    .as_deref(),
                Some("Review")
            );
        }
        assert!(set_folder(&conn, &id, Some("   ")).unwrap());
        assert_eq!(find_worktree(&conn, &id).unwrap().unwrap().folder, None);
        assert!(!set_folder(&conn, &WorkspaceId("comet/missing".into()), None).unwrap());
    }

    #[test]
    fn folders_survive_reopening_the_database_and_accept_eighty_unicode_characters() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state.db");
        let id = WorkspaceId("comet/work".into());
        let name = "試".repeat(80);
        {
            let conn = db::open(&path).unwrap();
            insert_project(&conn, &sample()).unwrap();
            conn.execute("INSERT INTO worktrees (project_name, name, branch, path) VALUES ('comet', 'work', 'main', '/tmp/work')", []).unwrap();
            assert!(set_folder(&conn, &id, Some(&name)).unwrap());
            assert!(set_archived(&conn, &id, true).unwrap());
        }
        let conn = db::open(&path).unwrap();
        let row = find_worktree(&conn, &id).unwrap().unwrap();
        assert_eq!(row.folder.as_deref(), Some(name.as_str()));
        assert!(row.archived);
        assert!(set_folder(&conn, &id, None).unwrap());
        assert_eq!(find_worktree(&conn, &id).unwrap().unwrap().folder, None);
    }

    #[test]
    fn projects_round_trip() {
        let conn = db::open_in_memory().unwrap();
        insert_project(&conn, &sample()).unwrap();
        let listed = list_projects(&conn).unwrap();
        assert_eq!(listed, vec![sample()]);
    }

    #[test]
    fn inserting_twice_updates_rather_than_duplicating() {
        let conn = db::open_in_memory().unwrap();
        insert_project(&conn, &sample()).unwrap();
        let mut moved = sample();
        moved.path = PathBuf::from("/elsewhere/comet");
        insert_project(&conn, &moved).unwrap();

        let listed = list_projects(&conn).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, PathBuf::from("/elsewhere/comet"));
    }

    #[test]
    fn a_status_note_is_written_bounded_and_cleared_by_a_blank() {
        let conn = db::open_in_memory().unwrap();
        insert_project(&conn, &sample()).unwrap();
        conn.execute(
            "INSERT INTO worktrees (project_name, name, branch, path) \
             VALUES ('comet', 'bright-harbor', 'bright-harbor', '/tmp/wt')",
            [],
        )
        .unwrap();
        let workspace = WorkspaceId("comet/bright-harbor".into());
        assert_eq!(status_note(&conn, &workspace).unwrap(), None);

        assert!(set_status_note(&conn, &workspace, Some("  waiting on CI\nlog"), 50).unwrap());
        assert_eq!(
            status_note(&conn, &workspace).unwrap(),
            Some(StatusNote {
                text: "waiting on CI".into(),
                set_at: 50
            })
        );

        assert!(set_status_note(&conn, &workspace, Some("   "), 60).unwrap());
        assert_eq!(status_note(&conn, &workspace).unwrap(), None);
        assert!(
            !set_status_note(&conn, &WorkspaceId("comet/gone".into()), Some("x"), 70).unwrap(),
            "an unknown workspace is not silently accepted"
        );
    }
}
