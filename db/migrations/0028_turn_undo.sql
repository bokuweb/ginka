-- Only fresh, fully captured turn boundaries are eligible for guarded undo.
ALTER TABLE checkpoints ADD COLUMN undo_state TEXT;
