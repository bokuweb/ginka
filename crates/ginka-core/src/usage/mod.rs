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
pub mod scan;

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
            ..UsageTotals::default()
        })
    })?;
    Ok(rows.next().transpose()?.unwrap_or_default())
}

/// What each day cost, newest first.
///
/// One row per session per day is summed, because a session's numbers are
/// cumulative: taking the last turn of each session on each day is what makes
/// the total mean anything.
pub fn by_day(conn: &Connection, days: u32, rates: Option<&RateTable>) -> Result<Vec<UsageRow>> {
    grouped(conn, &Grouping::Day, days, rates)
}

/// What each agent cost over the last `days`.
pub fn by_agent(conn: &Connection, days: u32, rates: Option<&RateTable>) -> Result<Vec<UsageRow>> {
    grouped(conn, &Grouping::Agent, days, rates)
}

/// What each login cost over the last `days`.
///
/// A session on an account since removed is still counted under it: the
/// cost was real, and the id is what it was known by. Work run outside Ginka
/// has no login to be filed under and is not here.
pub fn by_account(
    conn: &Connection,
    days: u32,
    rates: Option<&RateTable>,
) -> Result<Vec<UsageRow>> {
    grouped(conn, &Grouping::Account, days, rates)
}

/// What each model cost over the last `days`.
pub fn by_model(conn: &Connection, days: u32, rates: Option<&RateTable>) -> Result<Vec<UsageRow>> {
    grouped(conn, &Grouping::Model, days, rates)
}

/// What each project cost over the last `days`. `projects` is each
/// project's name and the folders that are its — its checkout and its
/// worktrees — which is how work run outside Ginka is placed; work in a
/// folder no project owns is named by the folder.
pub fn by_project(
    conn: &Connection,
    days: u32,
    rates: Option<&RateTable>,
    projects: &[(String, std::path::PathBuf)],
) -> Result<Vec<UsageRow>> {
    grouped(conn, &Grouping::Project(projects), days, rates)
}

/// What a report is grouped by.
enum Grouping<'a> {
    Day,
    Agent,
    Account,
    Model,
    Project(&'a [(String, std::path::PathBuf)]),
}

/// One session's usage, before it is grouped.
struct Spent {
    at: i64,
    agent: String,
    account: Option<String>,
    model: Option<String>,
    /// The Ginka workspace it ran in; absent for work run outside Ginka.
    workspace: Option<String>,
    /// The folder it ran in, for work run outside Ginka.
    cwd: Option<std::path::PathBuf>,
    /// The session, for counting what could not be priced once.
    session: String,
    totals: UsageTotals,
    /// Input already separated from cache reads, for pricing.
    tokens: TokenTotals,
}

impl Grouping<'_> {
    fn label(&self, spent: &Spent) -> Option<String> {
        Some(match self {
            Self::Day => {
                use chrono::TimeZone as _;
                chrono::Local
                    .timestamp_opt(spent.at, 0)
                    .single()?
                    .format("%Y-%m-%d")
                    .to_string()
            }
            Self::Agent => spent.agent.clone(),
            Self::Account => spent.account.clone()?,
            Self::Model => spent.model.clone().unwrap_or_default(),
            Self::Project(projects) => match (&spent.workspace, &spent.cwd) {
                (Some(workspace), _) => workspace
                    .split_once('/')
                    .map_or(workspace.as_str(), |(project, _)| project)
                    .to_string(),
                (None, Some(cwd)) => projects
                    .iter()
                    .filter(|(_, folder)| cwd.starts_with(folder))
                    .max_by_key(|(_, folder)| folder.as_os_str().len())
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| home_relative(cwd)),
                (None, None) => return None,
            },
        })
    }
}

/// A folder as a reader writes it: under their home, from `~`.
fn home_relative(folder: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => match folder.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => folder.display().to_string(),
        },
        None => folder.display().to_string(),
    }
}

/// Sum what was spent, grouped.
///
/// Ginka's own sessions count by their last turn, since a vendor reports a
/// session's totals cumulatively. Work run outside Ginka (`scan`) counts
/// request by request, less any conversation Ginka ran itself, whose vendor
/// log the scanner also reads. A session the vendor did not price is priced
/// from `rates` where the table knows its model, and the row says so; one
/// neither knows is counted as unpriced rather than as free (§3.3 N13).
fn grouped(
    conn: &Connection,
    grouping: &Grouping<'_>,
    days: u32,
    rates: Option<&RateTable>,
) -> Result<Vec<UsageRow>> {
    Ok(fold(&spent(conn, days)?, grouping, rates))
}

/// Every report at once, reading what was spent a single time: the usage
/// page asks for all of them, and a machine's own vendor logs are tens of
/// thousands of requests.
pub fn report(
    conn: &Connection,
    days: u32,
    rates: Option<&RateTable>,
    projects: &[(String, std::path::PathBuf)],
) -> Result<Report> {
    let spent = spent(conn, days)?;
    Ok(Report {
        by_day: fold(&spent, &Grouping::Day, rates),
        by_agent: fold(&spent, &Grouping::Agent, rates),
        by_account: fold(&spent, &Grouping::Account, rates),
        by_model: fold(&spent, &Grouping::Model, rates),
        by_project: fold(&spent, &Grouping::Project(projects), rates),
    })
}

/// The usage page's reports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub by_day: Vec<UsageRow>,
    pub by_agent: Vec<UsageRow>,
    pub by_account: Vec<UsageRow>,
    pub by_model: Vec<UsageRow>,
    pub by_project: Vec<UsageRow>,
}

/// What was spent over the last `days`, by Ginka and outside it.
fn spent(conn: &Connection, days: u32) -> Result<Vec<Spent>> {
    let cutoff = chrono::Utc::now().timestamp() - i64::from(days) * 86_400;
    let mut spent = Vec::new();

    let mut statement = conn.prepare(
        "SELECT last.at, last.agent, last.account_id, last.model, s.workspace_id,
                last.input_tokens, last.output_tokens,
                last.cache_read_tokens, last.reasoning_tokens,
                last.cost_usd, last.turn, last.session_id
           FROM (
             SELECT * FROM usage_events
              WHERE (session_id, turn) IN (
                SELECT session_id, max(turn) FROM usage_events GROUP BY session_id
              )
           ) AS last
           LEFT JOIN sessions s ON s.id = last.session_id
          WHERE last.at >= ?1",
    )?;
    let rows = statement.query_map([cutoff], |row| {
        let agent: String = row.get(1)?;
        let totals = UsageTotals {
            input_tokens: row.get(5)?,
            output_tokens: row.get(6)?,
            cache_read_tokens: row.get(7)?,
            reasoning_tokens: row.get(8)?,
            cost_usd: row.get(9)?,
            turns: row.get(10)?,
            estimated: false,
            unpriced: 0,
        };
        let tokens = TokenTotals {
            input: if input_counts_cache_reads(&agent) {
                totals.input_tokens.saturating_sub(totals.cache_read_tokens)
            } else {
                totals.input_tokens
            },
            output: totals.output_tokens,
            cache_read: totals.cache_read_tokens,
            cache_write: 0,
        };
        Ok(Spent {
            at: row.get(0)?,
            agent,
            account: row.get(2)?,
            model: row.get(3)?,
            workspace: row.get(4)?,
            cwd: None,
            session: row.get(11)?,
            totals,
            tokens,
        })
    })?;
    for row in rows {
        spent.push(row?);
    }

    let mut statement = conn.prepare(
        "SELECT at, agent, model, cwd, vendor_session,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens
           FROM outside_usage
          WHERE at >= ?1
            AND vendor_session NOT IN (
              SELECT vendor_session_id FROM sessions WHERE vendor_session_id IS NOT NULL
            )",
    )?;
    let rows = statement.query_map([cutoff], |row| {
        let tokens = TokenTotals {
            input: row.get(5)?,
            output: row.get(6)?,
            cache_read: row.get(7)?,
            cache_write: row.get(8)?,
        };
        Ok(Spent {
            at: row.get(0)?,
            agent: row.get(1)?,
            account: None,
            model: row.get(2)?,
            workspace: None,
            cwd: Some(std::path::PathBuf::from(row.get::<_, String>(3)?)),
            session: row.get(4)?,
            totals: UsageTotals {
                // Reported the way Ginka's own are: input with its cache
                // reads beside it, not inside it.
                input_tokens: tokens.input,
                output_tokens: tokens.output,
                cache_read_tokens: tokens.cache_read,
                ..UsageTotals::default()
            },
            tokens,
        })
    })?;
    for row in rows {
        spent.push(row?);
    }
    Ok(spent)
}

/// Group what was spent. Days, agents and logins come in label order —
/// newest day first; models and projects lead with what spent the most.
fn fold(spent: &[Spent], grouping: &Grouping<'_>, rates: Option<&RateTable>) -> Vec<UsageRow> {
    let mut groups: std::collections::BTreeMap<String, UsageTotals> = Default::default();
    let mut unpriced: std::collections::BTreeSet<(String, String)> = Default::default();
    for session in spent {
        let Some(label) = grouping.label(session) else {
            continue;
        };
        let cost = session
            .totals
            .cost_usd
            .map(|cost| (cost, false))
            .or_else(|| {
                rates?
                    .cost_of(session.model.as_deref()?, &session.tokens)
                    .map(|cost| (cost, true))
            });
        let group = groups.entry(label.clone()).or_default();
        group.input_tokens += session.totals.input_tokens;
        group.output_tokens += session.totals.output_tokens;
        group.cache_read_tokens += session.totals.cache_read_tokens;
        group.reasoning_tokens += session.totals.reasoning_tokens;
        group.turns += session.totals.turns;
        match cost {
            Some((cost, estimated)) => {
                group.cost_usd = Some(group.cost_usd.unwrap_or(0.0) + cost);
                group.estimated |= estimated;
            }
            // Counted once per session, however many requests it made.
            None => {
                if unpriced.insert((label, session.session.clone())) {
                    group.unpriced += 1;
                }
            }
        }
    }
    let mut rows: Vec<UsageRow> = groups
        .into_iter()
        .rev()
        .map(|(label, totals)| UsageRow { label, totals })
        .collect();
    if matches!(grouping, Grouping::Model | Grouping::Project(_)) {
        rows.sort_by_key(|row| {
            std::cmp::Reverse(
                row.totals.input_tokens + row.totals.output_tokens + row.totals.cache_read_tokens,
            )
        });
    }
    rows
}

/// Keep usage read from vendor logs; a request already kept is skipped.
/// Returns how many were new.
pub fn store_outside(conn: &Connection, records: &[scan::Record]) -> Result<usize> {
    // One transaction: a first scan is every request on the machine.
    let transaction = conn.unchecked_transaction()?;
    let mut added = 0;
    for record in records {
        added += transaction.execute(
            "INSERT OR IGNORE INTO outside_usage
                (key, agent, vendor_session, model, cwd,
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                record.key,
                record.agent,
                record.vendor_session,
                record.model,
                record.cwd.to_string_lossy(),
                record.tokens.input,
                record.tokens.output,
                record.tokens.cache_read,
                record.tokens.cache_write,
                record.at,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(added)
}

/// Where each vendor log has been read to.
pub fn watermarks(conn: &Connection) -> Result<std::collections::HashMap<String, scan::Watermark>> {
    let mut statement = conn.prepare("SELECT file, state FROM usage_scan_state")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut marks = std::collections::HashMap::new();
    for row in rows {
        let (file, state) = row?;
        if let Ok(mark) = serde_json::from_str(&state) {
            marks.insert(file, mark);
        }
    }
    Ok(marks)
}

/// Remember where a vendor log has been read to.
pub fn save_watermark(conn: &Connection, file: &str, mark: &scan::Watermark) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_scan_state (file, state) VALUES (?1, ?2)
         ON CONFLICT(file) DO UPDATE SET state = excluded.state",
        rusqlite::params![file, serde_json::to_string(mark)?],
    )?;
    Ok(())
}

/// Whether an agent's input count already includes the tokens read from its
/// cache. Codex reports cached input as part of input; Anthropic's API, and
/// so Claude Code, reports it beside it.
fn input_counts_cache_reads(agent: &str) -> bool {
    agent == "codex"
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
            reasoning_effort: None,
            service_tier: None,
            state: SessionState::Finished,
            title: None,
            summary: None,
            vendor_session_id: None,
            access_mode: Default::default(),
            origin: None,
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

        let agents = by_agent(&conn, 30, None).unwrap();
        let claude = agents.iter().find(|row| row.label == "claude").unwrap();
        assert_eq!(claude.totals.input_tokens, 300, "not 400");
        let codex = agents.iter().find(|row| row.label == "codex").unwrap();
        assert_eq!(codex.totals.input_tokens, 50);

        // And a day's worth is the same numbers under a date.
        let days = by_day(&conn, 30, None).unwrap();
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
        assert!(by_agent(&conn, 30, None).unwrap().is_empty());
        assert!(!by_agent(&conn, 365, None).unwrap().is_empty());
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
        let rows = by_account(&conn, 30, None).unwrap();
        let find = |id: &str| rows.iter().find(|row| row.label == id).unwrap();
        assert_eq!(find("claude-work").totals.input_tokens, 300);
        assert_eq!(find("claude").totals.input_tokens, 100);
        // And the agent view still sums both logins.
        assert_eq!(
            by_agent(&conn, 30, None).unwrap()[0].totals.input_tokens,
            400
        );
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

    fn unpriced(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            cost_usd: None,
        }
    }

    #[test]
    fn a_turn_the_vendor_did_not_price_is_priced_from_the_table_and_says_so() {
        let conn = db::open_in_memory().unwrap();
        let now = chrono::Utc::now().timestamp();
        let reported = session_in(&conn, "a", "claude");
        let codex = session_in(&conn, "b", "codex");
        let unknown = session_in(&conn, "c", "codex");
        let account = AccountId("x".into());
        record(
            &conn,
            &reported,
            1,
            "claude",
            &account,
            Some("opus"),
            &usage(10, 10, 0.5),
            now,
        )
        .unwrap();
        record(
            &conn,
            &codex,
            1,
            "codex",
            &account,
            Some("gpt-5"),
            &unpriced(1_000_000, 0),
            now,
        )
        .unwrap();
        record(
            &conn,
            &unknown,
            1,
            "codex",
            &account,
            Some("mystery"),
            &unpriced(5, 5),
            now,
        )
        .unwrap();

        let mut rates = RateTable::empty(now);
        rates.insert(
            "gpt-5",
            ModelRate {
                input_per_million: 1.25,
                output_per_million: 10.0,
                cache_read_per_million: 0.0,
                cache_write_per_million: 0.0,
            },
        );

        let agents = by_agent(&conn, 30, Some(&rates)).unwrap();
        let claude = agents.iter().find(|row| row.label == "claude").unwrap();
        assert_eq!(claude.totals.cost_usd, Some(0.5));
        assert!(!claude.totals.estimated, "the vendor's own figure");
        let codex = agents.iter().find(|row| row.label == "codex").unwrap();
        assert_eq!(codex.totals.cost_usd, Some(1.25));
        assert!(codex.totals.estimated, "priced from the table");
        assert_eq!(
            codex.totals.unpriced, 1,
            "the model the table does not know"
        );

        // Without a table the same rows are unpriced, not free.
        let bare = by_agent(&conn, 30, None).unwrap();
        let codex = bare.iter().find(|row| row.label == "codex").unwrap();
        assert_eq!(codex.totals.cost_usd, None);
        assert_eq!(codex.totals.unpriced, 2);
    }

    fn outside(
        key: &str,
        session: &str,
        agent: &'static str,
        model: &str,
        cwd: &str,
        input: u64,
    ) -> scan::Record {
        scan::Record {
            key: key.into(),
            agent,
            vendor_session: session.into(),
            model: model.into(),
            cwd: cwd.into(),
            tokens: TokenTotals {
                input,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            at: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn usage_run_outside_ginka_is_counted_once_and_never_twice() {
        let conn = db::open_in_memory().unwrap();
        let now = chrono::Utc::now().timestamp();
        // A session Ginka ran, whose vendor log the scanner will also find.
        let mine = session_in(&conn, "mine", "claude");
        session::set_vendor_session_id(&conn, &mine, "vendor-mine").unwrap();
        record(
            &conn,
            &mine,
            1,
            "claude",
            &AccountId("claude".into()),
            Some("opus"),
            &usage(100, 0, 1.0),
            now,
        )
        .unwrap();

        let records = [
            outside(
                "claude:a",
                "terminal-1",
                "claude",
                "claude-opus-5-5",
                "/work/comet/src",
                1_000,
            ),
            outside(
                "claude:b",
                "terminal-1",
                "claude",
                "claude-opus-5-5",
                "/work/comet",
                2_000,
            ),
            outside(
                "codex:c",
                "terminal-2",
                "codex",
                "gpt-6-sol",
                "/elsewhere",
                5_000,
            ),
            // Ginka's own conversation, as its vendor logged it.
            outside(
                "claude:d",
                "vendor-mine",
                "claude",
                "opus",
                "/work/comet",
                100,
            ),
        ];
        assert_eq!(store_outside(&conn, &records).unwrap(), 4);
        assert_eq!(
            store_outside(&conn, &records).unwrap(),
            0,
            "a request read twice is one"
        );

        let agents = by_agent(&conn, 30, None).unwrap();
        let claude = agents.iter().find(|row| row.label == "claude").unwrap();
        assert_eq!(
            claude.totals.input_tokens,
            100 + 1_000 + 2_000,
            "not 100 twice"
        );
        let codex = agents.iter().find(|row| row.label == "codex").unwrap();
        assert_eq!(codex.totals.input_tokens, 5_000);
        assert_eq!(codex.totals.unpriced, 1, "one session nothing could price");

        let models = by_model(&conn, 30, None).unwrap();
        let labels: Vec<&str> = models.iter().map(|row| row.label.as_str()).collect();
        assert!(
            labels.contains(&"claude-opus-5-5")
                && labels.contains(&"gpt-6-sol")
                && labels.contains(&"opus"),
            "{labels:?}"
        );

        let projects = [("comet".to_string(), std::path::PathBuf::from("/work/comet"))];
        let by_project = by_project(&conn, 30, None, &projects).unwrap();
        let comet = by_project.iter().find(|row| row.label == "comet").unwrap();
        assert_eq!(
            comet.totals.input_tokens,
            100 + 1_000 + 2_000,
            "Ginka's session in the project, and the runs in its folder"
        );
        assert!(
            by_project.iter().any(|row| row.label == "/elsewhere"),
            "a folder no project owns is named by its path"
        );

        // Models and projects lead with what spent the most.
        assert_eq!(models[0].label, "gpt-6-sol", "{models:?}");
        assert_eq!(by_project[0].label, "/elsewhere", "{by_project:?}");

        // Nothing run outside Ginka has an account to file it under.
        let accounts = by_account(&conn, 30, None).unwrap();
        assert_eq!(
            accounts
                .iter()
                .map(|row| row.totals.input_tokens)
                .sum::<u64>(),
            100
        );
    }
}
