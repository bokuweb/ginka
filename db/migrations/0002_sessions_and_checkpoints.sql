-- Agent sessions, their transcripts, and the checkpoints taken between turns.
--
-- A session is addressed by the workspace id rather than by a foreign key into
-- `worktrees`: a workspace is keyed by (project, immutable name), and history
-- has to survive the worktree being removed from disk. An orphaned session is
-- unreachable from the sidebar but is still the record of what an agent did.

CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    workspace_id      TEXT NOT NULL,
    agent             TEXT NOT NULL,
    model             TEXT,
    state             TEXT NOT NULL,
    summary           TEXT,
    -- The vendor's own id for the conversation. Without it a session can be
    -- replayed but not continued.
    vendor_session_id TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

-- The sidebar reads the most recent session per workspace on every repaint.
CREATE INDEX sessions_by_workspace ON sessions (workspace_id, updated_at DESC);

-- The transcript: an append-only log of normalized events, not rendered
-- messages. `seq` is both the order and the pagination cursor.
CREATE TABLE session_events (
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    at         INTEGER NOT NULL,
    -- A `TranscriptPayload` as JSON. Stored whole rather than decomposed into
    -- columns: the event shapes are the protocol's to change, and a migration
    -- per variant would make adding one a schema change.
    payload    TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE checkpoints (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL,
    turn         INTEGER NOT NULL,
    -- The commit holding the snapshot. It is on no branch: restoring reads the
    -- tree out of it, so the user's own history is never rewritten.
    commit_id    TEXT NOT NULL,
    label        TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE INDEX checkpoints_by_workspace ON checkpoints (workspace_id, created_at DESC);
