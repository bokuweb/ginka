-- Keep tags in the note row so an edit remains one atomic write. Existing
-- notes start without tags and remain readable after this migration.
ALTER TABLE notes ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
