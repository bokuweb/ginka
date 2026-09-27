-- A line of status an agent (or a person) writes on a workspace: what it is
-- doing, or what it is waiting on. Presentation state owned by Ginka, like
-- `pinned` and `archived`, so git reconciliation never touches it.
ALTER TABLE worktrees ADD COLUMN status_note TEXT;
ALTER TABLE worktrees ADD COLUMN status_note_at INTEGER;
