-- A chat job may continue an existing conversation instead of starting one
-- (MonoCode's session reminders): its prompt is sent to this session,
-- queued if the agent is working.
ALTER TABLE cron_jobs ADD COLUMN session TEXT;
