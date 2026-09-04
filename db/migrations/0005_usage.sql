-- What each turn cost, as the vendor reported it.
--
-- The events already arrive on every session; until now they were folded into
-- a view model and forgotten. Kept per turn rather than per session so a long
-- conversation can say which part of it was expensive, and stamped with the
-- agent and model because the same session can change both.
CREATE TABLE usage_events (
    session_id    TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    turn          INTEGER NOT NULL,
    agent         TEXT NOT NULL,
    model         TEXT,
    input_tokens      INTEGER NOT NULL,
    output_tokens     INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    reasoning_tokens  INTEGER NOT NULL,
    -- Absent for a vendor that does not price a request; a zero here would be
    -- a claim that it was free.
    cost_usd      REAL,
    at            INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn)
);

CREATE INDEX usage_by_time ON usage_events (at DESC);
