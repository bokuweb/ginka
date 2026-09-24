-- Usage from agents run outside Ginka, read from the vendors' own session
-- logs (roadmap §4.4's scanner), and where each log has been read to.
--
-- One row per request, keyed so reading a log twice adds nothing. Kept apart
-- from `usage_events`, whose rows are Ginka's sessions and cumulative; a
-- conversation Ginka ran itself appears in its vendor's log too, and is left
-- out when reports add the two up, by its vendor session id.
CREATE TABLE outside_usage (
    key                TEXT PRIMARY KEY,
    agent              TEXT NOT NULL,
    vendor_session     TEXT NOT NULL,
    model              TEXT,
    cwd                TEXT NOT NULL,
    input_tokens       INTEGER NOT NULL,
    output_tokens      INTEGER NOT NULL,
    cache_read_tokens  INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    at                 INTEGER NOT NULL
);

CREATE INDEX outside_usage_by_time ON outside_usage (at DESC);

-- The watermark per log file: its byte offset and what the file had said by
-- then about its session, as JSON.
CREATE TABLE usage_scan_state (
    file  TEXT PRIMARY KEY,
    state TEXT NOT NULL
);
