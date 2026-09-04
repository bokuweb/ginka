-- Transcript persistence. The daemon owns it: a client holds no messages, so
-- search has to be answerable here (docs/roadmap.md §3.3 N10).

CREATE TABLE sessions (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    provider     TEXT NOT NULL,
    -- Two title columns rather than one, so an agent's title can never
    -- overwrite a name the user typed. See §3.3 N5.
    user_title   TEXT,
    agent_title  TEXT,
    -- True while `agent_title` holds prompt text rather than a real title.
    agent_title_is_placeholder INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;

CREATE INDEX sessions_workspace_idx ON sessions (workspace_id);

CREATE TABLE messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    -- Per-session position, so a transcript can be paged without depending on
    -- the global insert order.
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('user', 'agent', 'system')),
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX messages_session_seq_uq ON messages (session_id, seq);
