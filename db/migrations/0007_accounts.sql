-- Which login a session ran on, and the latest reading of each login's
-- rate-limit windows (`docs/accounts.md` §9).
--
-- The account record itself is configuration in `settings.json`, not a row
-- here, so `account_id` is text rather than a foreign key: a session outlives
-- the account it ran on and still says which one that was. Every row written
-- before this migration ran on what is now the provider's default account,
-- whose id is the provider's own, and the backfill says so rather than leaving
-- the column nullable.

ALTER TABLE sessions ADD COLUMN account_id TEXT NOT NULL DEFAULT '';
UPDATE sessions SET account_id = provider WHERE account_id = '';

ALTER TABLE usage_events ADD COLUMN account_id TEXT NOT NULL DEFAULT '';
UPDATE usage_events SET account_id = agent WHERE account_id = '';

CREATE INDEX usage_by_account ON usage_events (account_id, at DESC);

-- A gauge, not an event: one row per account, overwritten by each reading.
-- `windows` is the vendor's set of windows as JSON, because the set and its
-- labels are the vendor's and a column per window would be a migration per
-- vendor.
CREATE TABLE plan_snapshots (
    account_id  TEXT PRIMARY KEY,
    plan        TEXT,
    windows     TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    -- `reported` by a turn, or `fetched` on request.
    source      TEXT NOT NULL
);
