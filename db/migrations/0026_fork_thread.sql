-- A conversation forked from the end of another inherits its vendor thread,
-- and its next resume asks the agent to branch that thread rather than
-- continue it, so the original and the fork never write into each other.
-- Cleared once the agent reports the branch's own id.
ALTER TABLE sessions ADD COLUMN fork_thread INTEGER NOT NULL DEFAULT 0;
