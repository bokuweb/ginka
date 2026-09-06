-- What a session moved from another agent still has to be told.
--
-- A fork onto a different agent (or a different login) cannot inherit the
-- vendor's thread: it lives in the other vendor's own store, in the other
-- account's directory. What can be carried across is the record — the daemon
-- holds every transcript in one normalized shape — so the fork is seeded with
-- a digest of it on its first turn. The digest is kept here rather than in the
-- transcript because it is addressed to the agent, not to the reader, and is
-- cleared once the agent has a thread of its own (`vendor_session_id` set):
-- a first turn that never connected is retried with the digest intact.

ALTER TABLE sessions ADD COLUMN handoff TEXT;
