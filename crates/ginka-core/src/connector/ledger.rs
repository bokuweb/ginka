//! What a connector remembers across a restart.
//!
//! Two small tables (`docs/connectors.md` §7). `connector_deliveries` is
//! written *before* a post and marked after it, so a daemon that dies in
//! between sends the reply again on boot rather than losing twenty minutes
//! of an agent's work. `connector_seen` is the dedup set, so a redelivered
//! envelope after a restart is still a duplicate.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _};
use std::path::Path;
use std::sync::Mutex;

/// A connector's own handle on the ledger tables.
///
/// The daemon's service owns its connection; a connector opens a second one
/// on the same file, which in WAL mode is the ordinary case. Wrapped so the
/// host never has to name the database crate.
pub struct Ledger {
    conn: Mutex<Connection>,
}

impl Ledger {
    /// Open the daemon's database for the ledger tables.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            conn: Mutex::new(crate::db::open(path)?),
        })
    }

    /// An in-memory ledger, for tests.
    pub fn in_memory() -> Result<Self> {
        Ok(Self {
            conn: Mutex::new(crate::db::open_in_memory()?),
        })
    }

    /// Run one operation against the connection.
    pub fn with<T>(&self, run: impl FnOnce(&Connection) -> T) -> T {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        run(&conn)
    }
}

/// One thing a connector meant to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub id: i64,
    pub connector: String,
    pub channel: String,
    pub thread: String,
    /// The [`super::fold::Outbound`] as JSON.
    pub payload: String,
    pub attempts: u32,
    pub created_at: i64,
}

/// How long an undelivered reply is still worth sending, in seconds.
pub const REDELIVER_WITHIN_SECS: i64 = 24 * 3_600;
/// How many times a reply is tried before it is given up on.
pub const MAX_ATTEMPTS: u32 = 3;
/// How many seen messages a connector keeps.
pub const SEEN_KEPT: usize = 4_096;

/// Note something about to be posted. Answers with the row to mark later.
pub fn record(
    conn: &Connection,
    connector: &str,
    channel: &str,
    thread: &str,
    payload: &str,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO connector_deliveries (connector, channel, thread, payload, attempts, created_at)
         VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        rusqlite::params![connector, channel, thread, payload, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// It was posted.
pub fn delivered(conn: &Connection, id: i64, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE connector_deliveries SET delivered_at = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

/// It was tried again and still did not go.
pub fn attempted(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE connector_deliveries SET attempts = attempts + 1 WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// What is still owed to a thread: recent enough, and not tried too often.
pub fn undelivered(conn: &Connection, connector: &str, now: i64) -> Result<Vec<Delivery>> {
    let mut statement = conn.prepare(
        "SELECT id, connector, channel, thread, payload, attempts, created_at
           FROM connector_deliveries
          WHERE connector = ?1 AND delivered_at IS NULL
            AND created_at > ?2 AND attempts < ?3
          ORDER BY id",
    )?;
    let rows = statement.query_map(
        rusqlite::params![connector, now - REDELIVER_WITHIN_SECS, MAX_ATTEMPTS],
        |row| {
            Ok(Delivery {
                id: row.get(0)?,
                connector: row.get(1)?,
                channel: row.get(2)?,
                thread: row.get(3)?,
                payload: row.get(4)?,
                attempts: row.get::<_, i64>(5)? as u32,
                created_at: row.get(6)?,
            })
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Forget deliveries older than the redelivery window. They were either
/// sent or given up on; either way they are history.
pub fn prune_deliveries(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM connector_deliveries WHERE created_at <= ?1",
        [now - REDELIVER_WITHIN_SECS],
    )?)
}

/// Whether a message has been handled already.
pub fn seen(conn: &Connection, connector: &str, channel: &str, message: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM connector_seen WHERE connector = ?1 AND channel = ?2 AND message = ?3",
            rusqlite::params![connector, channel, message],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Note a message as handled. Answers `false` when it already was, which is
/// the atomic form of "have I seen this": two envelopes for one message
/// cannot both get `true`.
pub fn mark_seen(
    conn: &Connection,
    connector: &str,
    channel: &str,
    message: &str,
    now: i64,
) -> Result<bool> {
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO connector_seen (connector, channel, message, seen_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![connector, channel, message, now],
    )?;
    if inserted > 0 {
        conn.execute(
            "DELETE FROM connector_seen
              WHERE connector = ?1 AND rowid NOT IN (
                    SELECT rowid FROM connector_seen WHERE connector = ?1
                     ORDER BY seen_at DESC, rowid DESC LIMIT ?2)",
            rusqlite::params![connector, SEEN_KEPT as i64],
        )?;
    }
    Ok(inserted > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn a_reply_is_owed_until_it_is_marked_delivered() {
        let conn = db::open_in_memory().unwrap();
        let id = record(
            &conn,
            "slack",
            "C1",
            "1.0",
            r#"{"kind":"note","text":"hi"}"#,
            100,
        )
        .unwrap();
        let owed = undelivered(&conn, "slack", 200).unwrap();
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0].id, id);
        assert_eq!(owed[0].attempts, 1);
        delivered(&conn, id, 201).unwrap();
        assert!(undelivered(&conn, "slack", 300).unwrap().is_empty());
    }

    #[test]
    fn a_reply_is_given_up_on_after_three_tries_or_a_day() {
        let conn = db::open_in_memory().unwrap();
        let id = record(&conn, "slack", "C1", "1.0", "{}", 100).unwrap();
        attempted(&conn, id).unwrap();
        attempted(&conn, id).unwrap();
        assert!(
            undelivered(&conn, "slack", 200).unwrap().is_empty(),
            "three attempts is the limit"
        );
        let stale = record(&conn, "slack", "C1", "1.0", "{}", 100).unwrap();
        assert!(
            undelivered(&conn, "slack", 100 + REDELIVER_WITHIN_SECS + 1)
                .unwrap()
                .is_empty()
        );
        assert!(stale > id);
        assert_eq!(
            prune_deliveries(&conn, 100 + REDELIVER_WITHIN_SECS + 1).unwrap(),
            2
        );
    }

    #[test]
    fn a_message_is_seen_exactly_once_and_the_set_is_bounded() {
        let conn = db::open_in_memory().unwrap();
        assert!(mark_seen(&conn, "slack", "C1", "1.0", 1).unwrap());
        assert!(
            !mark_seen(&conn, "slack", "C1", "1.0", 2).unwrap(),
            "a redelivery"
        );
        assert!(seen(&conn, "slack", "C1", "1.0").unwrap());
        assert!(!seen(&conn, "slack", "C1", "1.1").unwrap());
        for index in 0..(SEEN_KEPT + 10) {
            mark_seen(
                &conn,
                "slack",
                "C1",
                &format!("2.{index}"),
                10 + index as i64,
            )
            .unwrap();
        }
        let kept: i64 = conn
            .query_row("SELECT count(*) FROM connector_seen", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept as usize, SEEN_KEPT);
        assert!(
            !seen(&conn, "slack", "C1", "1.0").unwrap(),
            "the oldest went first"
        );
    }
}
