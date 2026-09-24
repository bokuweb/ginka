-- Scheduled prompts and commands (roadmap §4.4 `cronjobs`), and their runs.
--
-- `checked_at` is the moment up to which the schedule has been honoured: a
-- due time after it is still owed. Runs missed while the daemon was down are
-- not replayed one by one — the next tick fires once and moves it on.
-- `last_session` / `last_terminal` are what the previous run started, for
-- overlap-skip.
CREATE TABLE cron_jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    project       TEXT NOT NULL,
    workspace     TEXT,
    name          TEXT NOT NULL,
    schedule      TEXT NOT NULL,
    via           TEXT NOT NULL,
    agent         TEXT,
    body          TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1,
    checked_at    INTEGER NOT NULL,
    last_session  TEXT,
    last_terminal TEXT
);

CREATE TABLE cron_runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job         INTEGER NOT NULL REFERENCES cron_jobs(id) ON DELETE CASCADE,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    outcome     TEXT NOT NULL,
    detail      TEXT
);

CREATE INDEX cron_runs_by_job ON cron_runs (job, started_at DESC);
