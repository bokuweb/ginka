-- A shell probe a scheduled job runs first (Orca's precheck): a non-zero exit
-- skips that firing, so "review new PRs every hour" does not start an agent
-- in an hour with no new PRs.
ALTER TABLE cron_jobs ADD COLUMN precheck TEXT;
