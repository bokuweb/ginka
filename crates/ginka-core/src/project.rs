use anyhow::Result;
use ginka_protocol::{ProjectName, WorkspaceId};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A registered repository or folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: ProjectName,
    pub path: PathBuf,
    pub default_branch: String,
    pub label: Option<String>,
    pub sort_order: i64,
    pub kind: ProjectKind,
    /// `None` means "not probed yet", and is treated as `true` so the first CI
    /// poll after a cold boot still runs.
    pub has_origin: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    /// Worktree per workspace, branches, PR/CI features.
    Git,
    /// A plain folder: one implicit workspace, git features off.
    Plain,
}

impl ProjectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Plain => "plain",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "plain" => Self::Plain,
            _ => Self::Git,
        }
    }
}

/// One git worktree; the unit of isolation a workspace is scoped to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Worktree {
    pub project: ProjectName,
    /// Immutable identity, assigned once at creation. See `WorkspaceId`.
    pub name: String,
    /// The live branch, reconciled against git on each sync tick.
    pub branch: String,
    pub path: PathBuf,
    pub head: Option<String>,
    pub pinned: bool,
}

impl Worktree {
    pub fn workspace_id(&self) -> WorkspaceId {
        WorkspaceId::new(&self.project, &self.name)
    }
}

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

    #[test]
    fn workspace_id_survives_a_branch_switch() {
        let mut worktree = Worktree {
            project: ProjectName("comet".into()),
            name: "bright-harbor".into(),
            branch: "bright-harbor".into(),
            path: PathBuf::from("/tmp/wt"),
            head: None,
            pinned: false,
        };
        let before = worktree.workspace_id();
        // An agent checks out a different branch inside the worktree.
        worktree.branch = "some/other-branch".into();
        assert_eq!(worktree.workspace_id(), before);
    }
}
