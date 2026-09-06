-- Where a session came from, and what a chat connector remembers across a
-- restart (docs/connectors.md §7).
--
-- The origin is three columns rather than one because the thread-to-session
-- lookup is by all three, and the unique index over them is what stops two
-- sessions answering one thread. `origin_active` is how a thread is told
-- "start fresh": the old session keeps its origin for the sidebar's chip but
-- stops being the one a reply finds.
--
-- `access_mode` arrives here because a connector is the first caller that
-- fixes it per session; every row written before ran at the default, and the
-- backfill says so.

ALTER TABLE sessions ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'ask';
ALTER TABLE sessions ADD COLUMN origin_connector TEXT;
ALTER TABLE sessions ADD COLUMN origin_channel TEXT;
ALTER TABLE sessions ADD COLUMN origin_thread TEXT;
ALTER TABLE sessions ADD COLUMN origin_active INTEGER NOT NULL DEFAULT 1;

CREATE UNIQUE INDEX sessions_origin_uq
    ON sessions (origin_connector, origin_channel, origin_thread)
    WHERE origin_connector IS NOT NULL AND origin_active = 1;

-- Written before a post and marked after it, so a daemon that dies in
-- between sends the reply again on boot rather than losing it.
CREATE TABLE connector_deliveries (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    connector    TEXT NOT NULL,
    channel      TEXT NOT NULL,
    thread       TEXT NOT NULL,
    -- The outbound as JSON, in the connector's own shape.
    payload      TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 1,
    created_at   INTEGER NOT NULL,
    delivered_at INTEGER
);

CREATE INDEX connector_deliveries_owed
    ON connector_deliveries (connector, delivered_at, created_at);

-- The bounded dedup set: a redelivered envelope after a restart is still a
-- duplicate.
CREATE TABLE connector_seen (
    connector TEXT NOT NULL,
    channel   TEXT NOT NULL,
    message   TEXT NOT NULL,
    seen_at   INTEGER NOT NULL,
    PRIMARY KEY (connector, channel, message)
);
