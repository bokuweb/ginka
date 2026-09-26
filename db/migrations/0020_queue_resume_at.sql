-- When a queue held by a usage limit lets itself go: the window's reset,
-- plus a margin. NULL for a queue held by a person, which only a person lets
-- go of.
ALTER TABLE queue_state ADD COLUMN resume_at INTEGER;
