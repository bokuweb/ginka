-- What the user was in the middle of typing, per workspace.
--
-- A prompt half-written when the window closed is work, and losing it to a
-- restart is the kind of small betrayal that stops people trusting a tool with
-- anything long. Keyed by workspace because that is what a composer belongs to;
-- there is at most one draft per workspace, so the id is the key.
CREATE TABLE composer_drafts (
    workspace_id TEXT PRIMARY KEY,
    text         TEXT NOT NULL,
    updated_at   INTEGER NOT NULL
);
