-- Saved shell commands and prompts: Orca's Quick Commands.
--
-- `project` is NULL for one that belongs to every project. Keyed by the
-- project's name, like every other table, so removing a project does not
-- take the reader's commands with it.
CREATE TABLE quick_commands (
    id      TEXT PRIMARY KEY,
    project TEXT,
    name    TEXT NOT NULL,
    kind    TEXT NOT NULL,
    body    TEXT NOT NULL
);
