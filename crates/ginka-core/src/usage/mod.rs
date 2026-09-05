//! What the work cost.
//!
//! Every driver already reports usage; until this it was folded into a view
//! model and forgotten. Recorded per turn rather than per session, so a long
//! conversation can say which part of it was expensive, and stamped with the
//! agent and model because one session can change both.
//!
//! Vendors report cumulative totals for a session rather than per-turn deltas
//! (`AgentEvent::Usage` says so). A row is therefore the session's total *as
//! of* that turn, and a total across sessions is the sum of each one's last
//! word — not of every row, which would count the same tokens once per turn.

pub mod pricing;

pub use pricing::{
    CostQuality, DaySlice, ModelRate, ModelSlice, PlanUsage, PlanWindow, RateTable, TokenTotals,
    UsageEvent, UsageSummary, summarize,
};

use anyhow::Result;
use ginka_protocol::model::{PlanSnapshot, PlanSource, UsageRow, UsageTotals};
use ginka_protocol::{AccountId, SessionId, Usage};
use rusqlite::{Connection, OptionalExtension as _};

/// Record what a turn had cost by the time it ended, and on which login.
#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    session: &SessionId,
    turn: u32,
    agent: &str,
    account: &AccountId,
    model: Option<&str>,
    usage: &Usage,
    at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_events
            (session_id, turn, agent, account_id, model, input_tokens, output_tokens,
             cache_read_tokens, reasoning_tokens, cost_usd, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(session_id, turn) DO UPDATE SET
             input_tokens = excluded.input_tokens,
             output_tokens = excluded.output_tokens,
             cache_read_tokens = excluded.cache_read_tokens,
             reasoning_tokens = excluded.reasoning_tokens,
             cost_usd = excluded.cost_usd,
             at = excluded.at",
        rusqlite::params![
            session.0,
            turn,
            agent,
            account.0,
            model,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.reasoning_tokens,
            usage.cost_usd,
            at,
        ],
    )?;
    Ok(())
}

/// Keep the latest reading of an account's rate-limit windows.
///
/// One row per account, replaced rather than appended: a gauge's history is
/// not what anyone asks for, and the usage events already say what was spent.
pub fn record_plan(
    conn: &Connection,
    account: &AccountId,
    usage: &ginka_protocol::model::PlanUsage,
    at: i64,
    source: PlanSource,
) -> Result<PlanSnapshot> {
    conn.execute(
        "INSERT INTO plan_snapshots (account_id, plan, windows, observed_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id) DO UPDATE SET
             plan = excluded.plan,
             windows = excluded.windows,
             observed_at = excluded.observed_at,
             source = excluded.source",
        rusqlite::params![
            account.0,
            usage.plan,
            serde_json::to_string(&usage.windows)?,
            at,
            source_str(source),
        ],
    )?;
    Ok(PlanSnapshot {
        account: account.clone(),
        usage: usage.clone(),
        observed_at: at,
        source,
    })
}

/// The latest reading of every account that has one, newest first.
pub fn plans(conn: &Connection) -> Result<Vec<PlanSnapshot>> {
    let mut statement = conn.prepare(
        "SELECT account_id, plan, windows, observed_at, source
           FROM plan_snapshots
          ORDER BY observed_at DESC",
    )?;
    let rows = statement.query_map([], read_plan)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The latest reading of one account, if it has ever been read.
pub fn plan_for(conn: &Connection, account: &AccountId) -> Result<Option<PlanSnapshot>> {
    let mut statement = conn.prepare(
        "SELECT account_id, plan, windows, observed_at, source
           FROM plan_snapshots
          WHERE account_id = ?1",
    )?;
    Ok(statement.query_row([&account.0], read_plan).optional()?)
}

fn read_plan(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanSnapshot> {
    let windows: String = row.get(2)?;
    Ok(PlanSnapshot {
        account: AccountId(row.get(0)?),
        usage: ginka_protocol::model::PlanUsage {
            plan: row.get(1)?,
            windows: serde_json::from_str(&windows).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        },
        observed_at: row.get(3)?,
        source: match row.get::<_, String>(4)?.as_str() {
            "fetched" => PlanSource::Fetched,
            _ => PlanSource::Reported,
        },
    })
}

fn source_str(source: PlanSource) -> &'static str {
    match source {
        PlanSource::Reported => "reported",
        PlanSource::Fetched => "fetched",
    }
}

/// What one session has cost.
pub fn for_session(conn: &Connection, session: &SessionId) -> Result<UsageTotals> {
    let mut statement = conn.prepare(
        "SELECT input_tokens, output_tokens, cache_read_tokens, reasoning_tokens,
                cost_usd, turn
           FROM usage_events
          WHERE session_id = ?1
          ORDER BY turn DESC
          LIMIT 1",
    )?;
    let mut rows = statement.query_map([&session.0], |row| {
        Ok(UsageTotals {
            input_tokens: row.get(0)?,
            output_tokens: row.get(1)?,
            cache_read_tokens: row.get(2)?,
            reasoning_tokens: row.get(3)?,
            cost_usd: row.get(4)?,
            turns: row.get(5)?,
        })
    })?;
    Ok(rows.next().transpose()?.unwrap_or_default())
}

/// What each day cost, newest first.
///
/// One row per session per day is summed, because a session's numbers are
/// cumulative: taking the last turn of each session on each day is what makes
/// the total mean anything.
pub fn by_day(conn: &Connection, days: u32) -> Result<Vec<UsageRow>> {
    grouped(conn, "date(last.at, 'unixepoch', 'localtime')", Some(days))
}

/// What each agent cost over the last `days`.
pub fn by_agent(conn: &Connection, days: u32) -> Result<Vec<UsageRow>> {
    grouped(conn, "last.agent", Some(days))
}

/// What each login cost over the last `days`.
///
/// A session on an account since removed is still counted under it: the
/// cost was real, and the id is what it was known by.
pub fn by_account(conn: &Connection, days: u32) -> Result<Vec<UsageRow>> {
    grouped(conn, "last.account_id", Some(days))
}

/// Sum the last turn of every session, grouped by `label`.
fn grouped(conn: &Connection, label: &str, days: Option<u32>) -> Result<Vec<UsageRow>> {
    // The inner query takes each session's highest turn — its cumulative total
    // — and the outer one groups those.
    let cutoff = days
        .map(|days| format!("WHERE last.at >= strftime('%s', 'now', '-{days} days')"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {label} AS label,
                sum(last.input_tokens), sum(last.output_tokens),
                sum(last.cache_read_tokens), sum(last.reasoning_tokens),
                sum(last.cost_usd), sum(last.turn)
           FROM (
             SELECT * FROM usage_events
              WHERE (session_id, turn) IN (
                SELECT session_id, max(turn) FROM usage_events GROUP BY session_id
              )
           ) AS last
           {cutoff}
          GROUP BY label
          ORDER BY label DESC"
    );

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok(UsageRow {
            label: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            totals: UsageTotals {
                input_tokens: row.get::<_, Option<u64>>(1)?.unwrap_or_default(),
                output_tokens: row.get::<_, Option<u64>>(2)?.unwrap_or_default(),
                cache_read_tokens: row.get::<_, Option<u64>>(3)?.unwrap_or_default(),
                reasoning_tokens: row.get::<_, Option<u64>>(4)?.unwrap_or_default(),
                cost_usd: row.get(5)?,
                turns: row.get::<_, Option<u32>>(6)?.unwrap_or_default(),
            },
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Forget usage older than `days`.
///
/// The roadmap's retention sweep (§4.4). Cost history is interesting for a
/// month and clutter forever.
pub fn sweep(conn: &Connection, days: u32) -> Result<usize> {
    Ok(conn.execute(
        &format!("DELETE FROM usage_events WHERE at < strftime('%s', 'now', '-{days} days')"),
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::session;
    use ginka_protocol::WorkspaceId;
    use ginka_protocol::model::{Session, SessionState};

    fn session_in(conn: &Connection, id: &str, agent: &str) -> SessionId {
        let session = Session {
            id: SessionId(id.into()),
            workspace: WorkspaceId("comet/harbor".into()),
            agent: agent.into(),
            account: AccountId(agent.into()),
            model: Some("opus".into()),
            state: SessionState::Finished,
            title: None,
            summary: None,
            vendor_session_id: None,
            created_at: 0,
            updated_at: 0,
        };
        session::insert(conn, &session).unwrap();
        session.id
    }

    fn usage(input: u64, output: u64, cost: f64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            cost_usd: Some(cost),
        }
    }

    #[test]
    fn a_sessions_cost_is_its_latest_word_rather_than_every_word() {
        // Vendors report cumulative totals, so adding up the turns would count
        // the same tokens once per turn.
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "claude");
        record(
            &conn,
            &session,
            1,
            "claude",
            &AccountId("claude".into()),
            Some("opus"),
            &usage(100, 10, 0.01),
            1_000,
        )
        .unwrap();
        record(
            &conn,
            &session,
            2,
            "claude",
            &AccountId("claude".into()),
            Some("opus"),
            &usage(250, 40, 0.03),
            2_000,
        )
        .unwrap();

        let totals = for_session(&conn, &session).unwrap();
        assert_eq!(totals.input_tokens, 250);
        assert_eq!(totals.output_tokens, 40);
        assert_eq!(totals.cost_usd, Some(0.03));
        assert_eq!(totals.turns, 2);
    }

    #[test]
    fn a_turn_reported_twice_is_recorded_once() {
        // A vendor that repeats its accounting must not double the bill.
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "claude");
        record(
            &conn,
            &session,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(100, 10, 0.01),
            1_000,
        )
        .unwrap();
        record(
            &conn,
            &session,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(120, 12, 0.012),
            1_100,
        )
        .unwrap();

        let totals = for_session(&conn, &session).unwrap();
        assert_eq!(totals.input_tokens, 120, "the later word wins");
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn cost_is_summed_across_sessions_but_not_across_their_turns() {
        let conn = db::open_in_memory().unwrap();
        let one = session_in(&conn, "one", "claude");
        let two = session_in(&conn, "two", "codex");
        // Stamped now, because the grouped views are windowed by time and a
        // timestamp from 1970 falls outside every window there is.
        let now = chrono::Utc::now().timestamp();
        record(
            &conn,
            &one,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(100, 10, 0.01),
            now,
        )
        .unwrap();
        record(
            &conn,
            &one,
            2,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(300, 30, 0.05),
            now,
        )
        .unwrap();
        record(
            &conn,
            &two,
            1,
            "codex",
            &AccountId("codex".into()),
            None,
            &usage(50, 5, 0.002),
            now,
        )
        .unwrap();

        let agents = by_agent(&conn, 30).unwrap();
        let claude = agents.iter().find(|row| row.label == "claude").unwrap();
        assert_eq!(claude.totals.input_tokens, 300, "not 400");
        let codex = agents.iter().find(|row| row.label == "codex").unwrap();
        assert_eq!(codex.totals.input_tokens, 50);

        // And a day's worth is the same numbers under a date.
        let days = by_day(&conn, 30).unwrap();
        assert_eq!(days.len(), 1, "it all happened today: {days:?}");
        assert_eq!(days[0].totals.input_tokens, 350);
    }

    #[test]
    fn work_outside_the_window_is_not_counted() {
        // "What did this month cost" must not quietly include last year.
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "claude");
        let now = chrono::Utc::now().timestamp();
        record(
            &conn,
            &session,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(999, 999, 9.99),
            now - 60 * 60 * 24 * 200,
        )
        .unwrap();
        assert!(by_agent(&conn, 30).unwrap().is_empty());
        assert!(!by_agent(&conn, 365).unwrap().is_empty());
    }

    #[test]
    fn a_vendor_that_does_not_price_its_work_reports_no_cost() {
        // Zero would be a claim that it was free.
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "codex");
        record(
            &conn,
            &session,
            1,
            "codex",
            &AccountId("codex".into()),
            None,
            &Usage {
                input_tokens: 10,
                ..Usage::default()
            },
            1_000,
        )
        .unwrap();
        assert_eq!(for_session(&conn, &session).unwrap().cost_usd, None);
    }

    #[test]
    fn a_session_with_no_usage_reported_costs_nothing_rather_than_failing() {
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "claude");
        assert_eq!(
            for_session(&conn, &session).unwrap(),
            UsageTotals::default()
        );
    }

    #[test]
    fn old_usage_is_swept_and_recent_usage_is_kept() {
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "claude");
        let now = chrono::Utc::now().timestamp();
        record(
            &conn,
            &session,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(1, 1, 0.0),
            now - 60 * 60 * 24 * 90,
        )
        .unwrap();
        record(
            &conn,
            &session,
            2,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(2, 2, 0.0),
            now,
        )
        .unwrap();

        assert_eq!(sweep(&conn, 30).unwrap(), 1);
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn cost_is_attributed_to_the_login_that_paid_it_even_once_it_is_gone() {
        let conn = db::open_in_memory().unwrap();
        let work = session_in(&conn, "w", "claude");
        let personal = session_in(&conn, "p", "claude");
        let now = chrono::Utc::now().timestamp();
        record(
            &conn,
            &work,
            1,
            "claude",
            &AccountId("claude-work".into()),
            None,
            &usage(300, 30, 0.05),
            now,
        )
        .unwrap();
        record(
            &conn,
            &personal,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(100, 10, 0.01),
            now,
        )
        .unwrap();

        // No account record exists for `claude-work` in this database — it
        // was configuration, and it has been removed — and the row still
        // says who paid.
        let rows = by_account(&conn, 30).unwrap();
        let find = |id: &str| rows.iter().find(|row| row.label == id).unwrap();
        assert_eq!(find("claude-work").totals.input_tokens, 300);
        assert_eq!(find("claude").totals.input_tokens, 100);
        // And the agent view still sums both logins.
        assert_eq!(by_agent(&conn, 30).unwrap()[0].totals.input_tokens, 400);
    }

    #[test]
    fn a_plan_reading_replaces_the_last_one_and_keeps_its_age() {
        use ginka_protocol::model::{PlanUsage, PlanWindow};
        let conn = db::open_in_memory().unwrap();
        let account = AccountId("codex".into());
        let first = PlanUsage {
            plan: Some("pro".into()),
            windows: vec![PlanWindow {
                label: "week".into(),
                used_percent: 40.0,
                resets_at: Some(1_789_141_311),
            }],
        };
        record_plan(&conn, &account, &first, 1_000, PlanSource::Fetched).unwrap();
        let second = PlanUsage {
            windows: vec![PlanWindow {
                label: "week".into(),
                used_percent: 41.0,
                resets_at: Some(1_789_141_311),
            }],
            ..first.clone()
        };
        let snapshot = record_plan(&conn, &account, &second, 2_000, PlanSource::Reported).unwrap();
        assert_eq!(snapshot.observed_at, 2_000);
        assert_eq!(snapshot.source, PlanSource::Reported);

        let stored = plans(&conn).unwrap();
        assert_eq!(stored.len(), 1, "a gauge, not a log");
        assert_eq!(stored[0], snapshot);
        assert_eq!(stored[0].usage.plan.as_deref(), Some("pro"));
        assert_eq!(stored[0].usage.windows[0].used_percent, 41.0);
        assert_eq!(plan_for(&conn, &account).unwrap(), Some(snapshot));
        assert_eq!(plan_for(&conn, &AccountId("claude".into())).unwrap(), None);
    }

    #[test]
    fn removing_a_session_takes_its_usage_with_it() {
        let conn = db::open_in_memory().unwrap();
        let session = session_in(&conn, "s", "claude");
        record(
            &conn,
            &session,
            1,
            "claude",
            &AccountId("claude".into()),
            None,
            &usage(1, 1, 0.0),
            1_000,
        )
        .unwrap();
        session::remove(&conn, &session).unwrap();
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }
}
