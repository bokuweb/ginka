-- Archived workspaces remain registered and keep their worktree, sessions and
-- checkpoints. This is presentation state owned by Ginka, so reconciliation
-- updates branch/head without touching it.
ALTER TABLE worktrees ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
