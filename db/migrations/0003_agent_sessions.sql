-- Bring the sessions table up to what the daemon's session layer needs.
--
-- `sessions` and its two title columns arrived in 0002. This adds what running
-- an agent against it requires — which vendor, which model, what state, and the
-- vendor's own conversation id, without which a session can be replayed but not
-- continued — and the transcript and checkpoint tables beside it.
--
-- Which agent runs a session is already `provider` from 0002; the defaults on
-- the columns added here are for rows written before this migration, of which
-- there are none in practice — a NOT NULL column added to an existing table
-- needs one.

ALTER TABLE sessions ADD COLUMN model TEXT;
ALTER TABLE sessions ADD COLUMN state TEXT NOT NULL DEFAULT 'idle';
ALTER TABLE sessions ADD COLUMN summary TEXT;
ALTER TABLE sessions ADD COLUMN vendor_session_id TEXT;

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

-- Searching a transcript reads every event of every session in a workspace, so
-- the column it filters on is worth an index even though the payload itself is
-- matched with LIKE.
CREATE INDEX session_events_by_time ON session_events (at DESC);

-- `messages` in 0002 was a second, weaker transcript store: role and text only,
-- where this one keeps the whole normalized event. One transcript, not two.
DROP TABLE IF EXISTS messages;

CREATE TABLE checkpoints (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL,
    turn         INTEGER NOT NULL,
    -- The commit holding the state the turn ended at. It is on no branch:
    -- restoring reads the tree out of it, so the user's own history is never
    -- rewritten.
    commit_id    TEXT NOT NULL,
    -- The state the turn was handed, captured before the agent ran, so a hand
    -- edit made between turns is not attributed to the agent (§3.3 N8).
    start_commit TEXT,
    -- The commit HEAD pointed at when the turn started.
    base_commit  TEXT,
    label        TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE INDEX checkpoints_by_workspace ON checkpoints (workspace_id, created_at DESC);
