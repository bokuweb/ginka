-- What was read from each conversation file an agent's own CLI keeps
-- (`cli_sessions`), so listing a workspace's CLI conversations reads only the
-- files that changed since, across daemon restarts too. One row per file and
-- directory asked about: `agent` is NULL when the file holds no conversation
-- started in that directory, which is also worth remembering. The files are
-- the vendors'; this is only an index of them, and nothing here is read back
-- without the file's size and modification time still matching.
CREATE TABLE cli_session_files (
    path              TEXT NOT NULL,
    cwd               TEXT NOT NULL,
    modified_ns       INTEGER,
    len               INTEGER NOT NULL,
    agent             TEXT,
    vendor_session_id TEXT,
    title             TEXT,
    prompts           INTEGER,
    updated_at        INTEGER,
    PRIMARY KEY (path, cwd)
);
