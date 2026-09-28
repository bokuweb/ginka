//! Cron schedules: when a scheduled prompt or command is next due.
//!
//! The five fields every cron reads — minute, hour, day of month, month, day
//! of week — with lists, ranges and steps, and the `@hourly` family. Times are
//! wall-clock times in whatever zone the caller gives, because "every weekday
//! at nine" means nine where the reader is. A day of month and a day of week
//! both restricted fire on either, as in every cron since Vixie's. One-time
//! jobs use an absolute timestamp and are disabled when their firing is claimed.

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Utc};
use ginka_protocol::model::{CronJob, CronOutcome, CronRun, CronVia};
use ginka_protocol::{ProjectName, WorkspaceId};
use rusqlite::{Connection, OptionalExtension as _};

/// The parsed schedule, stored in the existing column for both kinds of job.
/// `@once` is distinct from every supported named recurring schedule.
enum JobSchedule {
    Recurring(Schedule),
    Once(DateTime<Utc>),
}

impl JobSchedule {
    fn parse(expression: &str) -> Result<Self> {
        let expression = expression.trim();
        if expression == "@once" || expression.starts_with("@once ") {
            let timestamp = expression.strip_prefix("@once").unwrap().trim();
            let at = DateTime::parse_from_rfc3339(timestamp).map_err(|_| {
                anyhow::anyhow!("@once needs an RFC 3339 timestamp with a time zone")
            })?;
            return Ok(Self::Once(at.with_timezone(&Utc)));
        }
        Ok(Self::Recurring(Schedule::parse(expression)?))
    }

    fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        match self {
            Self::Recurring(schedule) => schedule.next_after(after),
            Self::Once(at) if *at > after.with_timezone(&Utc) => {
                Some(at.with_timezone(&after.timezone()))
            }
            Self::Once(_) => None,
        }
    }

    fn is_once(&self) -> bool {
        matches!(self, Self::Once(_))
    }
}

/// Whether a one-time job has a recorded firing at or after its scheduled time.
/// A manual run before the deadline does not complete the schedule.
pub fn is_completed(job: &CronJob) -> bool {
    if job.enabled {
        return false;
    }
    let (Ok(JobSchedule::Once(at)), Some(run)) = (JobSchedule::parse(&job.schedule), &job.last_run)
    else {
        return false;
    };
    run.started_at >= at.timestamp()
}

/// A parsed cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    /// Bit `n` for day `n`, 1–31.
    days: u32,
    /// Bit `n` for month `n`, 1–12.
    months: u16,
    /// Bit `n` for weekday `n`, Sunday = 0.
    weekdays: u8,
    days_restricted: bool,
    weekdays_restricted: bool,
}

impl Schedule {
    /// Read a five-field expression or one of `@hourly`, `@daily`
    /// (`@midnight`), `@weekly`, `@monthly`, `@yearly` (`@annually`).
    pub fn parse(expression: &str) -> Result<Self> {
        let expression = match expression.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            named if named.starts_with('@') => bail!("{named} is not a schedule cron knows"),
            fields => fields,
        };
        let fields: Vec<&str> = expression.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            bail!(
                "a schedule has five fields — minute, hour, day, month, weekday — not {}",
                fields.len()
            );
        };
        // Weekday 7 is Sunday as well as 0.
        let weekdays = field(weekday, "weekday", 0, 7)?;
        let weekdays = (weekdays & 0x7f) | u64::from(weekdays & (1 << 7) != 0);
        Ok(Self {
            minutes: field(minute, "minute", 0, 59)?,
            hours: field(hour, "hour", 0, 23)? as u32,
            days: field(day, "day", 1, 31)? as u32,
            months: field(month, "month", 1, 12)? as u16,
            weekdays: weekdays as u8,
            days_restricted: day != "*",
            weekdays_restricted: weekday != "*",
        })
    }

    /// The first time strictly after `after` that this schedule fires.
    ///
    /// `None` only for a schedule that can never fire, such as the 31st of
    /// February.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        let zone = after.timezone();
        let local = after.naive_local();
        // The next whole minute: a schedule fires on minutes, strictly after.
        let mut at = local.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        // Four years covers every day-of-month and weekday pairing, the 29th
        // of February included; beyond that the schedule never fires.
        let limit = local + Duration::days(4 * 366);
        while at <= limit {
            if !bit(u64::from(self.months), at.month()) {
                at = first_of_next_month(at.date())?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !self.day_matches(at.date()) {
                at = at.date().succ_opt()?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !bit(u64::from(self.hours), at.hour()) {
                at = at.with_minute(0)? + Duration::hours(1);
                continue;
            }
            if !bit(self.minutes, at.minute()) {
                at += Duration::minutes(1);
                continue;
            }
            // A wall-clock time a daylight-saving change skips does not
            // exist there; the next one that does is taken.
            if let Some(fired) = zone.from_local_datetime(&at).earliest() {
                return Some(fired);
            }
            at += Duration::minutes(1);
        }
        None
    }

    fn day_matches(&self, date: NaiveDate) -> bool {
        let day = bit(u64::from(self.days), date.day());
        let weekday = bit(
            u64::from(self.weekdays),
            date.weekday().num_days_from_sunday(),
        );
        match (self.days_restricted, self.weekdays_restricted) {
            (true, true) => day || weekday,
            _ => day && weekday,
        }
    }
}

fn bit(mask: u64, value: u32) -> bool {
    mask & (1 << value) != 0
}

fn first_of_next_month(date: NaiveDate) -> Option<NaiveDate> {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)
}

/// One field as a bit mask: `*`, `n`, `a-b`, any of those `/step`, and
/// comma-separated lists of them.
fn field(text: &str, name: &str, low: u32, high: u32) -> Result<u64> {
    let mut mask = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| anyhow::anyhow!("{name}: {step:?} is not a step"))?;
                if step == 0 {
                    bail!("{name}: a step of 0 never moves");
                }
                (range, step)
            }
            None => (part, 1),
        };
        let number = |value: &str| -> Result<u32> {
            let number: u32 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("{name}: {value:?} is not a number"))?;
            if !(low..=high).contains(&number) {
                bail!("{name}: {number} is outside {low}–{high}");
            }
            Ok(number)
        };
        let (start, end) = match range {
            "*" => (low, high),
            _ => match range.split_once('-') {
                Some((start, end)) => (number(start)?, number(end)?),
                // `5/15` runs from 5 to the end, as cron reads it.
                None if step > 1 => (number(range)?, high),
                None => {
                    let only = number(range)?;
                    (only, only)
                }
            },
        };
        if start > end {
            bail!("{name}: {start}-{end} runs backwards");
        }
        for value in (start..=end).step_by(step as usize) {
            mask |= 1 << value;
        }
    }
    Ok(mask)
}

// ---------------------------------------------------------------------------
// The jobs the daemon keeps (roadmap §4.4 `cronjobs`), and their runs.

/// A job's fields as a request gives them, before they are stored.
#[derive(Debug, Clone)]
pub struct Draft {
    pub project: ProjectName,
    pub workspace: Option<WorkspaceId>,
    pub name: String,
    pub schedule: String,
    pub via: CronVia,
    pub agent: Option<String>,
    pub body: String,
    pub precheck: Option<String>,
    pub enabled: bool,
}

/// What a job's last firing started, for overlap-skip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastStarted {
    pub session: Option<String>,
    pub terminal: Option<String>,
}

const COLUMNS: &str =
    "id, project, workspace, name, schedule, via, agent, body, enabled, checked_at, precheck";

/// Every job, or one project's, by name.
pub fn list(conn: &Connection, project: Option<&ProjectName>) -> Result<Vec<CronJob>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM cron_jobs
          WHERE ?1 IS NULL OR project = ?1
          ORDER BY project, name COLLATE NOCASE, id"
    ))?;
    let rows = statement
        .query_map([project.map(|project| project.0.clone())], row_to_job)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(job, checked_at)| finish_job(conn, job, checked_at))
        .collect()
}

/// One job.
pub fn get(conn: &Connection, id: i64) -> Result<Option<CronJob>> {
    let row = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM cron_jobs WHERE id = ?1"),
            [id],
            row_to_job,
        )
        .optional()?;
    row.map(|(job, checked_at)| finish_job(conn, job, checked_at))
        .transpose()
}

/// Save a job: new without an `id`, a replacement with one. The schedule
/// must parse and fire; the caller has checked the project and workspace.
/// Saving moves its clock to `now`, so a job edited to fire every minute does
/// not fire at once for every minute it was not.
pub fn save(conn: &Connection, id: Option<i64>, draft: &Draft, now: i64) -> Result<CronJob> {
    let name = draft.name.trim();
    if name.is_empty() {
        bail!("a scheduled job needs a name");
    }
    if draft.body.trim().is_empty() {
        bail!("a scheduled job needs something to run");
    }
    let schedule = JobSchedule::parse(&draft.schedule)?;
    let checked = Utc
        .timestamp_opt(now, 0)
        .single()
        .ok_or_else(|| anyhow::anyhow!("the job's checked time is outside the supported range"))?;
    if schedule.next_after(&checked).is_none() {
        bail!("{} never fires", draft.schedule.trim());
    }
    let agent = match draft.via {
        CronVia::Chat => Some(
            draft
                .agent
                .clone()
                .filter(|agent| !agent.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("a chat job needs an agent to start"))?,
        ),
        CronVia::Terminal => None,
    };
    let workspace = draft
        .workspace
        .as_ref()
        .map(|workspace| workspace.0.clone());
    let precheck = draft
        .precheck
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty());
    let id = match id {
        Some(id) => {
            let changed = conn.execute(
                "UPDATE cron_jobs SET project = ?1, workspace = ?2, name = ?3, schedule = ?4,
                        via = ?5, agent = ?6, body = ?7, enabled = ?8, checked_at = ?9,
                        precheck = ?11
                  WHERE id = ?10",
                rusqlite::params![
                    draft.project.0,
                    workspace,
                    name,
                    draft.schedule.trim(),
                    draft.via.as_str(),
                    agent,
                    draft.body,
                    draft.enabled,
                    now,
                    id,
                    precheck,
                ],
            )?;
            if changed == 0 {
                bail!("no scheduled job with id {id}");
            }
            id
        }
        None => {
            conn.execute(
                "INSERT INTO cron_jobs (project, workspace, name, schedule, via, agent, body,
                                        enabled, checked_at, precheck)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    draft.project.0,
                    workspace,
                    name,
                    draft.schedule.trim(),
                    draft.via.as_str(),
                    agent,
                    draft.body,
                    draft.enabled,
                    now,
                    precheck,
                ],
            )?;
            conn.last_insert_rowid()
        }
    };
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("the job was saved but cannot be read back"))
}

/// Forget a job and its runs. Forgetting one that is not there is not an
/// error.
pub fn remove(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM cron_jobs WHERE id = ?1", [id])?;
    Ok(())
}

/// The enabled jobs a tick at `now` owes a firing, and moves each one's clock
/// on to `now` — once, however many due times were missed.
pub fn take_due<Tz: TimeZone>(conn: &Connection, now: &DateTime<Tz>) -> Result<Vec<CronJob>> {
    let mut due = Vec::new();
    for job in list(conn, None)? {
        if !job.enabled {
            continue;
        }
        let checked_at: i64 = conn.query_row(
            "SELECT checked_at FROM cron_jobs WHERE id = ?1",
            [job.id],
            |row| row.get(0),
        )?;
        let Ok(schedule) = JobSchedule::parse(&job.schedule) else {
            continue;
        };
        let Some(checked) = now.timezone().timestamp_opt(checked_at, 0).single() else {
            continue;
        };
        if schedule
            .next_after(&checked)
            .is_some_and(|next| next <= *now)
        {
            let changed = conn.execute(
                "UPDATE cron_jobs SET checked_at = ?1, enabled = ?2
                  WHERE id = ?3 AND enabled = 1 AND checked_at = ?4",
                rusqlite::params![now.timestamp(), !schedule.is_once(), job.id, checked_at],
            )?;
            if changed != 0 {
                due.push(job);
            }
        }
    }
    Ok(due)
}

/// What the job's last firing started.
pub fn last_started(conn: &Connection, id: i64) -> Result<LastStarted> {
    Ok(conn.query_row(
        "SELECT last_session, last_terminal FROM cron_jobs WHERE id = ?1",
        [id],
        |row| {
            Ok(LastStarted {
                session: row.get(0)?,
                terminal: row.get(1)?,
            })
        },
    )?)
}

/// Remember what a firing started.
pub fn set_last_started(conn: &Connection, id: i64, started: &LastStarted) -> Result<()> {
    conn.execute(
        "UPDATE cron_jobs SET last_session = ?1, last_terminal = ?2 WHERE id = ?3",
        rusqlite::params![started.session, started.terminal, id],
    )?;
    Ok(())
}

/// Record one firing and return its id.
pub fn record_run(
    conn: &Connection,
    job: i64,
    at: i64,
    outcome: CronOutcome,
    detail: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO cron_runs (job, started_at, outcome, detail) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![job, at, outcome.as_str(), detail],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Change what a firing came to.
pub fn set_run(
    conn: &Connection,
    run: i64,
    outcome: CronOutcome,
    detail: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE cron_runs SET outcome = ?1, detail = ?2 WHERE id = ?3",
        rusqlite::params![outcome.as_str(), detail, run],
    )?;
    Ok(())
}

/// A terminal job's terminal closed.
pub fn finish_run(conn: &Connection, run: i64, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE cron_runs SET finished_at = ?1, outcome = ?2 WHERE id = ?3",
        rusqlite::params![at, CronOutcome::Finished.as_str(), run],
    )?;
    Ok(())
}

/// A job's firings, most recent first.
pub fn runs(conn: &Connection, job: i64, limit: u32) -> Result<Vec<CronRun>> {
    let mut statement = conn.prepare(
        "SELECT id, job, started_at, finished_at, outcome, detail FROM cron_runs
          WHERE job = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
    )?;
    let runs = statement
        .query_map(rusqlite::params![job, limit], |row| {
            Ok(CronRun {
                id: row.get(0)?,
                job: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                outcome: CronOutcome::parse(&row.get::<_, String>(4)?)
                    .unwrap_or(CronOutcome::Failed),
                detail: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(runs)
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<(CronJob, i64)> {
    Ok((
        CronJob {
            id: row.get(0)?,
            project: ProjectName(row.get(1)?),
            workspace: row.get::<_, Option<String>>(2)?.map(WorkspaceId),
            name: row.get(3)?,
            schedule: row.get(4)?,
            via: CronVia::parse(&row.get::<_, String>(5)?).unwrap_or(CronVia::Terminal),
            agent: row.get(6)?,
            body: row.get(7)?,
            enabled: row.get(8)?,
            precheck: row.get(10)?,
            next_run_at: None,
            last_run: None,
        },
        row.get(9)?,
    ))
}

/// Fill in what is derived: the next firing, on this host's clock, and the
/// last run.
fn finish_job(conn: &Connection, mut job: CronJob, checked_at: i64) -> Result<CronJob> {
    if job.enabled
        && let Ok(schedule) = JobSchedule::parse(&job.schedule)
        && let Some(checked) = Local.timestamp_opt(checked_at, 0).single()
    {
        job.next_run_at = schedule.next_after(&checked).map(|next| next.timestamp());
    }
    job.last_run = runs(conn, job.id, 1)?.into_iter().next();
    Ok(job)
}

/// What a job's precheck said about firing now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precheck {
    /// It exited zero, or the job has none: fire.
    Pass,
    /// Do not fire; the reason is kept on the skipped run.
    Skip(String),
}

/// How long a precheck may take before its silence counts as "no".
pub const PRECHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `command` through the user's login shell in `dir` and say whether the
/// job should fire.
///
/// A login shell for the same reason terminal jobs use one: the probe is
/// usually `gh …` or a script, and it needs the PATH the user's terminal has.
/// It runs with no stdin; the last line it printed becomes the skip reason,
/// because "exit 1" alone does not say which of three conditions failed. The
/// caller must not hold the service while this runs.
pub fn run_precheck(
    command: &str,
    dir: &std::path::Path,
    timeout: std::time::Duration,
) -> Precheck {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut child = match Command::new(shell)
        .args(["-lc", command])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Precheck::Skip(format!("the precheck could not start: {error}")),
    };
    // Drained on their own threads so a chatty probe cannot fill a pipe and
    // hang waiting for a reader that is waiting for it to exit.
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.by_ref().take(64 * 1024).read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as _));
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(error) => return Precheck::Skip(format!("the precheck was lost: {error}")),
        }
    };
    let Some(status) = status else {
        return Precheck::Skip(format!(
            "the precheck gave no answer within {}s",
            timeout.as_secs()
        ));
    };
    if status.success() {
        return Precheck::Pass;
    }
    let printed = format!(
        "{}\n{}",
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default()
    );
    let said = printed
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect::<String>());
    let code = status
        .code()
        .map_or_else(|| "a signal".to_string(), |code| code.to_string());
    Precheck::Skip(match said {
        Some(said) => format!("precheck exited {code}: {said}"),
        None => format!("precheck exited {code}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn a_claimed_one_time_job_stays_disabled_after_reopening_the_database() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("jobs.sqlite");
        let conn = crate::db::open(&path).unwrap();
        let before = at("2026-09-28T00:00:00Z");
        let due = at("2026-09-28T00:30:00Z");
        let job = save(
            &conn,
            None,
            &Draft {
                project: ProjectName("comet".into()),
                workspace: None,
                name: "reminder".into(),
                schedule: "@once 2026-09-28T09:30:00+09:00".into(),
                via: CronVia::Terminal,
                agent: None,
                body: "true".into(),
                precheck: None,
                enabled: true,
            },
            before.timestamp(),
        )
        .unwrap();
        assert_eq!(job.next_run_at, Some(due.timestamp()));
        assert!(take_due(&conn, &before).unwrap().is_empty());
        assert_eq!(take_due(&conn, &due).unwrap().len(), 1);
        drop(conn);

        let conn = crate::db::open(&path).unwrap();
        let stored = get(&conn, job.id).unwrap().unwrap();
        assert!(!stored.enabled);
        assert_eq!(stored.next_run_at, None);
        assert!(
            take_due(&conn, &(due + Duration::days(1)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_manual_run_before_the_deadline_does_not_mark_a_one_time_job_complete() {
        let conn = crate::db::open_in_memory().unwrap();
        let before = at("2026-09-28T00:00:00Z");
        let due = at("2026-09-28T00:30:00Z");
        let job = save(
            &conn,
            None,
            &Draft {
                project: ProjectName("comet".into()),
                workspace: None,
                name: "reminder".into(),
                schedule: "@once 2026-09-28T09:30:00+09:00".into(),
                via: CronVia::Terminal,
                agent: None,
                body: "true".into(),
                precheck: None,
                enabled: true,
            },
            before.timestamp(),
        )
        .unwrap();
        record_run(
            &conn,
            job.id,
            before.timestamp(),
            CronOutcome::Finished,
            None,
        )
        .unwrap();
        let mut paused = job.clone();
        paused.enabled = false;
        paused.last_run = get(&conn, job.id).unwrap().unwrap().last_run;
        assert!(!is_completed(&paused));

        record_run(&conn, job.id, due.timestamp(), CronOutcome::Finished, None).unwrap();
        paused.last_run = get(&conn, job.id).unwrap().unwrap().last_run;
        assert!(is_completed(&paused));
    }

    #[test]
    fn a_precheck_passes_on_zero_and_says_why_it_skipped_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let quick = std::time::Duration::from_secs(10);
        assert_eq!(run_precheck("test -d .", dir.path(), quick), Precheck::Pass);
        assert_eq!(
            run_precheck("echo 'no new pull requests' >&2; exit 3", dir.path(), quick),
            Precheck::Skip("precheck exited 3: no new pull requests".into())
        );
        std::fs::write(dir.path().join("flag"), "").unwrap();
        assert_eq!(
            run_precheck("test -f flag", dir.path(), quick),
            Precheck::Pass,
            "it runs in the job's checkout"
        );
    }

    #[test]
    fn a_precheck_that_does_not_answer_is_a_skip() {
        let dir = tempfile::tempdir().unwrap();
        let verdict = run_precheck("sleep 5", dir.path(), std::time::Duration::from_millis(300));
        assert!(
            matches!(&verdict, Precheck::Skip(why) if why.contains("no answer")),
            "{verdict:?}"
        );
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn next(expression: &str, after: &str) -> String {
        Schedule::parse(expression)
            .unwrap()
            .next_after(&at(after))
            .expect("it fires")
            .to_rfc3339()
    }

    #[test]
    fn a_step_fires_on_the_next_multiple() {
        assert_eq!(
            next("*/15 * * * *", "2026-09-24T10:07:30Z"),
            "2026-09-24T10:15:00+00:00"
        );
        assert_eq!(
            next("*/15 * * * *", "2026-09-24T10:45:00Z"),
            "2026-09-24T11:00:00+00:00",
            "strictly after"
        );
    }

    #[test]
    fn weekdays_at_nine_skip_the_weekend() {
        // 2026-09-25 is a Friday.
        assert_eq!(
            next("0 9 * * 1-5", "2026-09-25T10:00:00Z"),
            "2026-09-28T09:00:00+00:00"
        );
    }

    #[test]
    fn the_named_schedules_are_their_expressions() {
        assert_eq!(
            next("@daily", "2026-09-24T23:59:00Z"),
            "2026-09-25T00:00:00+00:00"
        );
        assert_eq!(
            next("@hourly", "2026-09-24T23:59:00Z"),
            "2026-09-25T00:00:00+00:00"
        );
        assert_eq!(
            next("@weekly", "2026-09-24T12:00:00Z"),
            "2026-09-27T00:00:00+00:00",
            "Sunday midnight"
        );
        assert_eq!(
            next("@monthly", "2026-09-24T12:00:00Z"),
            "2026-10-01T00:00:00+00:00"
        );
    }

    #[test]
    fn a_day_a_month_does_not_have_is_skipped_to_one_that_does() {
        assert_eq!(
            next("0 0 31 * *", "2026-02-01T00:00:00Z"),
            "2026-03-31T00:00:00+00:00"
        );
        assert!(
            Schedule::parse("0 0 31 2 *")
                .unwrap()
                .next_after(&at("2026-01-01T00:00:00Z"))
                .is_none(),
            "never"
        );
    }

    #[test]
    fn a_restricted_day_and_weekday_fire_on_either() {
        // The 1st, or any Sunday: after Wed 2026-09-02 the next is Sun 09-06.
        assert_eq!(
            next("0 0 1 * 0", "2026-09-02T00:00:00Z"),
            "2026-09-06T00:00:00+00:00"
        );
        assert_eq!(
            next("0 0 1 * 0", "2026-09-29T00:00:00Z"),
            "2026-10-01T00:00:00+00:00"
        );
    }

    #[test]
    fn seven_is_sunday_too_and_lists_mix_with_ranges() {
        assert_eq!(
            next("30 12 * * 7", "2026-09-24T00:00:00Z"),
            "2026-09-27T12:30:00+00:00"
        );
        assert_eq!(
            next("0 8,12-13 * * *", "2026-09-24T09:00:00Z"),
            "2026-09-24T12:00:00+00:00"
        );
        assert_eq!(
            next("0 1-23/11 * * *", "2026-09-24T02:00:00Z"),
            "2026-09-24T12:00:00+00:00"
        );
    }

    #[test]
    fn the_time_is_read_on_the_callers_clock() {
        let tokyo = FixedOffset::east_opt(9 * 3600).unwrap();
        let after = tokyo.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap();
        let fired = Schedule::parse("0 9 * * *")
            .unwrap()
            .next_after(&after)
            .unwrap();
        assert_eq!(fired.to_rfc3339(), "2026-09-25T09:00:00+09:00");
    }

    #[test]
    fn a_malformed_expression_is_refused_with_its_reason() {
        for bad in [
            "* * * *",
            "* * * * * *",
            "61 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-2 * * * *",
            "a * * * *",
            "@sometimes",
            "",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad:?} was accepted");
        }
    }
}
