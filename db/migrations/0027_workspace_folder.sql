-- Sidebar grouping belongs to the immutable workspace, not its live branch.
ALTER TABLE worktrees ADD COLUMN folder TEXT;
