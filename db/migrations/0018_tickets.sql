-- Tickets: work an agent noticed in passing and handed to the reader as a
-- card, to be started in its own session or dismissed.
--
-- Keyed by workspace id rather than by a foreign key, like every other table
-- here: archiving or removing a worktree does not rewrite history.
CREATE TABLE tickets (
    id            TEXT PRIMARY KEY,
    workspace     TEXT NOT NULL,
    from_session  TEXT,
    title         TEXT NOT NULL,
    summary       TEXT NOT NULL,
    prompt        TEXT NOT NULL,
    state         TEXT NOT NULL,
    session       TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX tickets_by_workspace ON tickets (workspace, state, created_at);
