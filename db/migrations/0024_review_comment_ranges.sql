-- A review comment may cover a range of lines (Orca's multi-line comments):
-- `line` is where it starts and `end_line` where it ends, NULL for one line.
ALTER TABLE review_comments ADD COLUMN end_line INTEGER;
