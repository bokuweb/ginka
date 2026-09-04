-- Comments left on a diff, waiting to be sent back to the agent.
--
-- Anchored to a file and a line rather than to a diff: the diff is regenerated
-- on every read and its hunks move, while "line 42 of src/main.rs" is what the
-- reader meant and what the agent can act on.
--
-- Kept per workspace rather than per session, because a review outlives the
-- session that produced the work: an agent can finish, the user can read the
-- diff over lunch, and the comments still belong to the same worktree.
CREATE TABLE review_comments (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    path         TEXT NOT NULL,
    -- The line on the side the reader was looking at. `NULL` is a comment on
    -- the file as a whole, which is a thing people write.
    line         INTEGER,
    -- Which side of the diff the line number belongs to.
    side         TEXT NOT NULL,
    text         TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE INDEX review_comments_by_workspace ON review_comments (workspace_id, path, line);
