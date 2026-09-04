use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Migrations, embedded at compile time and applied in array order.
///
/// Never edit a shipped migration — add a new one. The applied count is stored
/// in SQLite's `user_version`, so reordering or rewriting history silently
/// skips work on machines that already ran it.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_projects_and_worktrees",
        include_str!("../../../db/migrations/0001_projects_and_worktrees.sql"),
    ),
    (
        "0002_sessions_and_messages",
        include_str!("../../../db/migrations/0002_sessions_and_messages.sql"),
    ),
];

/// Open the database, applying any migrations the file has not seen.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut conn = Connection::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// An in-memory database with the full schema. For tests.
pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL so the daemon's writers never block the readers serving the UI.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let applied: usize = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if applied > MIGRATIONS.len() {
        anyhow::bail!(
            "database is at migration {applied} but this build only knows {}; \
             it was written by a newer Ginka",
            MIGRATIONS.len()
        );
    }

    for (index, (name, sql)) in MIGRATIONS.iter().enumerate().skip(applied) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("applying migration {name}"))?;
        // pragma_update cannot be parameterised, and `index` is a usize we
        // produced ourselves, so the format is safe.
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
        tracing::info!(migration = name, "applied migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_the_schema() {
        let conn = open_in_memory().unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(tables.contains(&"projects".to_string()));
        assert!(tables.contains(&"worktrees".to_string()));
    }

    #[test]
    fn migrating_is_idempotent_across_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ginka.db");
        let conn = open(&path).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len());
        drop(conn);

        // Re-opening must not re-run anything.
        let conn = open(&path).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len());
    }

    #[test]
    fn a_newer_database_is_refused_rather_than_downgraded() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ginka.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", MIGRATIONS.len() + 1)
                .unwrap();
        }
        let err = open(&path).unwrap_err();
        assert!(err.to_string().contains("newer Ginka"), "{err}");
    }

    #[test]
    fn foreign_keys_cascade_from_projects_to_worktrees() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO projects (name, path, default_branch) VALUES ('comet', '/tmp/comet', 'main')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (project_name, name, branch, path) \
             VALUES ('comet', 'bright-harbor', 'bright-harbor', '/tmp/wt')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM projects WHERE name = 'comet'", [])
            .unwrap();
        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM worktrees", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
