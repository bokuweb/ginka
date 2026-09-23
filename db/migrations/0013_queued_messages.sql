-- Follow-ups waiting behind a session's current turn.
--
-- Stored so a daemon restart does not lose what the reader typed. They come
-- back held (`paused`): a queue that fired prompts into a session nobody is
-- watching, the moment the daemon came back, would be a surprise.
CREATE TABLE queued_messages (
    session_id TEXT NOT NULL,
    id         INTEGER NOT NULL,
    position   INTEGER NOT NULL,
    text       TEXT NOT NULL,
    PRIMARY KEY (session_id, id)
);

-- One row per session that has ever queued: the next id to hand out, so an
-- id is never reused across a restart, and whether the queue is held.
CREATE TABLE queue_state (
    session_id TEXT PRIMARY KEY,
    next_id    INTEGER NOT NULL,
    paused     INTEGER NOT NULL
);
