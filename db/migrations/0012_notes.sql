-- Markdown notes, kept by the daemon (`docs/monocode-parity.md`).
--
-- A note belongs to a project or to none: something written before a project
-- is registered is still worth keeping. Keyed by the project's name rather
-- than a foreign key, like every other table here, so removing a project
-- does not take the reader's writing with it.
CREATE TABLE notes (
    id           TEXT PRIMARY KEY,
    project      TEXT,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE INDEX notes_by_project ON notes (project, updated_at);
