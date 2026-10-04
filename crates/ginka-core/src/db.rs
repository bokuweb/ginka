//! The SQLite store: opening the database and applying the embedded migrations.

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
    (
        "0003_agent_sessions",
        include_str!("../../../db/migrations/0003_agent_sessions.sql"),
    ),
    (
        "0004_composer_drafts",
        include_str!("../../../db/migrations/0004_composer_drafts.sql"),
    ),
    (
        "0005_usage",
        include_str!("../../../db/migrations/0005_usage.sql"),
    ),
    (
        "0006_review_comments",
        include_str!("../../../db/migrations/0006_review_comments.sql"),
    ),
    (
        "0007_accounts",
        include_str!("../../../db/migrations/0007_accounts.sql"),
    ),
    (
        "0008_connectors",
        include_str!("../../../db/migrations/0008_connectors.sql"),
    ),
    (
        "0009_handoff",
        include_str!("../../../db/migrations/0009_handoff.sql"),
    ),
    (
        "0010_workspace_archive",
        include_str!("../../../db/migrations/0010_workspace_archive.sql"),
    ),
    (
        "0011_session_model_options",
        include_str!("../../../db/migrations/0011_session_model_options.sql"),
    ),
    (
        "0012_notes",
        include_str!("../../../db/migrations/0012_notes.sql"),
    ),
    (
        "0013_queued_messages",
        include_str!("../../../db/migrations/0013_queued_messages.sql"),
    ),
    (
        "0014_quick_commands",
        include_str!("../../../db/migrations/0014_quick_commands.sql"),
    ),
    (
        "0015_cron_jobs",
        include_str!("../../../db/migrations/0015_cron_jobs.sql"),
    ),
    (
        "0016_outside_usage",
        include_str!("../../../db/migrations/0016_outside_usage.sql"),
    ),
    (
        "0017_browser_history",
        include_str!("../../../db/migrations/0017_browser_history.sql"),
    ),
    (
        "0018_workspace_status",
        include_str!("../../../db/migrations/0018_workspace_status.sql"),
    ),
    (
        "0019_cron_precheck",
        include_str!("../../../db/migrations/0019_cron_precheck.sql"),
    ),
    (
        "0020_queue_resume_at",
        include_str!("../../../db/migrations/0020_queue_resume_at.sql"),
    ),
    (
        "0021_tickets",
        include_str!("../../../db/migrations/0021_tickets.sql"),
    ),
    (
        "0022_note_tags",
        include_str!("../../../db/migrations/0022_note_tags.sql"),
    ),
    (
        "0023_conversation_commands",
        include_str!("../../../db/migrations/0023_conversation_commands.sql"),
    ),
    (
        "0024_review_comment_ranges",
        include_str!("../../../db/migrations/0024_review_comment_ranges.sql"),
    ),
    (
        "0025_cron_session_target",
        include_str!("../../../db/migrations/0025_cron_session_target.sql"),
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
    fn a_second_writer_waits_for_the_first_instead_of_failing() {
        // More than one connection writes the database — the service's, the
        // chat connectors' ledger, `ginka doctor` — and WAL still lets only
        // one write at a time. rusqlite's default busy timeout (5 s) is what
        // makes the second wait its turn; this pins it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ginka.db");
        let first = open(&path).unwrap();
        let second = open(&path).unwrap();
        first.execute_batch("BEGIN IMMEDIATE").unwrap();
        first
            .execute(
                "INSERT INTO projects (name, path, kind, default_branch) VALUES ('a', '/a', 'git', 'main')",
                [],
            )
            .unwrap();

        let writing = std::thread::spawn(move || {
            second.execute(
                "INSERT INTO projects (name, path, kind, default_branch) VALUES ('b', '/b', 'git', 'main')",
                [],
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        first.execute_batch("COMMIT").unwrap();

        assert_eq!(
            writing.join().unwrap().unwrap(),
            1,
            "the second write went in"
        );
        let count: i64 = first
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

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
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"session_events".to_string()));
        assert!(tables.contains(&"checkpoints".to_string()));
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
    fn existing_notes_gain_empty_tags_without_losing_their_content() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        let before_tags = MIGRATIONS
            .iter()
            .position(|(name, _)| *name == "0022_note_tags")
            .expect("the note tags migration is registered");
        for (_, sql) in &MIGRATIONS[..before_tags] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before_tags)
            .unwrap();
        conn.execute(
            "INSERT INTO notes (id, project, title, body, created_at, updated_at)
             VALUES ('n', 'ginka', 'Plan', 'Existing markdown', 1, 2)",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let note = crate::notes::get(&conn, "n").unwrap().unwrap();
        assert_eq!(note.title, "Plan");
        assert_eq!(note.body, "Existing markdown");
        assert!(note.tags.is_empty());
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
    fn rows_from_before_accounts_are_attributed_to_the_providers_default() {
        // A session that ran before there were accounts ran on the vendor's
        // own home — which is what the provider's default account is — and
        // the backfill has to say so rather than leave the column empty.
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        let before_accounts = MIGRATIONS
            .iter()
            .position(|(name, _)| *name == "0007_accounts")
            .expect("the accounts migration is registered");
        for (_, sql) in &MIGRATIONS[..before_accounts] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before_accounts)
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, provider, created_at, updated_at)
             VALUES ('s', 'comet/harbor', 'codex', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usage_events (session_id, turn, agent, input_tokens, output_tokens,
                                       cache_read_tokens, reasoning_tokens, at)
             VALUES ('s', 1, 'codex', 1, 1, 0, 0, 1)",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let session: String = conn
            .query_row(
                "SELECT account_id FROM sessions WHERE id = 's'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session, "codex");
        let usage: String = conn
            .query_row(
                "SELECT account_id FROM usage_events WHERE session_id = 's'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(usage, "codex");
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
