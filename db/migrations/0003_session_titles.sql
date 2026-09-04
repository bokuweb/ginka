-- What a conversation is about, as opposed to what it is doing right now.
--
-- `summary` is the agent's latest line and changes on every turn; a title is
-- how the user finds this conversation again a week later. Taken from the
-- opening prompt unless the user renames it.
ALTER TABLE sessions ADD COLUMN title TEXT;

-- Searching a transcript reads every event of every session in a workspace, so
-- the column it filters on is worth an index even though the payload itself is
-- matched with LIKE.
CREATE INDEX session_events_by_time ON session_events (at DESC);
