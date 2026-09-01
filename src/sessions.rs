//! Reading the registered projects and their worktrees into sidebar rows.
//!
//! Blocking work -- SQLite and one `git status` per worktree -- so every caller
//! runs it off the main thread. It lives here rather than in `ginka-ui` because
//! it touches storage, and rather than in `main` because the shell refreshes on
//! a tick.

use ginka_core::{Paths, db, git, project, registry};
use ginka_ui::workspace::SessionRow;

/// Read the registered projects and their worktrees into sidebar rows.
///
/// Storage problems are logged and yield an empty list rather than stopping the
/// launch: an app that will not open is a worse failure than one that opens
/// empty and says so, and the empty state tells the user how to register a
/// project.
pub fn load(paths: &Paths) -> Vec<SessionRow> {
    let conn = match db::open(&paths.database()) {
        Ok(conn) => conn,
        Err(error) => {
            tracing::error!(%error, "could not open the database; starting with no sessions");
            return Vec::new();
        }
    };

    let projects = match project::list_projects(&conn) {
        Ok(projects) => projects,
        Err(error) => {
            tracing::error!(%error, "could not read projects");
            return Vec::new();
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let mut rows = Vec::new();
    for project in projects {
        // Adopt anything created outside the app since we last looked. A failure
        // here is not fatal: show what is stored rather than nothing.
        if let Err(error) = registry::sync_worktrees(&conn, &project) {
            tracing::warn!(project = %project.name, %error, "could not sync worktrees");
        }
        match project::list_worktrees(&conn, &project.name) {
            Ok(worktrees) => {
                for worktree in worktrees {
                    // A worktree whose directory has gone missing under us
                    // still gets a row; it reports no status rather than
                    // failing the whole load.
                    let status = git::branch_status(&worktree.path).unwrap_or_default();
                    let last_commit = git::last_commit_time(&worktree.path);
                    rows.push(SessionRow::from_worktree(
                        &project,
                        &worktree,
                        status,
                        last_commit,
                        now,
                    ));
                }
            }
            Err(error) => {
                tracing::warn!(project = %project.name, %error, "could not list worktrees")
            }
        }
    }
    rows
}
