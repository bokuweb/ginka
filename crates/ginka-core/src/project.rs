//! Persistence for projects and worktrees.
//!
//! The rows themselves are `ginka-protocol` types: the CLI and the app read
//! them straight off the wire, so there is one definition of what a project is
//! rather than a domain copy and a wire copy that drift.

use anyhow::Result;
use ginka_protocol::ProjectName;
use rusqlite::Connection;
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
        "SELECT project_name, name, branch, path, head, pinned
         FROM worktrees WHERE project_name = ?1 ORDER BY pinned DESC, name",
    )?;
    let rows = statement.query_map([&project.0], |row| {
        Ok(Worktree {
            project: ProjectName(row.get(0)?),
            name: row.get(1)?,
            branch: row.get(2)?,
            path: PathBuf::from(row.get::<_, String>(3)?),
            head: row.get(4)?,
            pinned: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
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
}
