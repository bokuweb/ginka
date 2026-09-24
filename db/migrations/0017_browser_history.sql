-- The browser surface's history per workspace (roadmap §4.4), for the
-- address bar to complete from by frecency. URLs are stored as
-- `browser_history::keepable` leaves them: no credentials, no fragments, no
-- secret-looking query parameters.
CREATE TABLE browser_history (
    workspace       TEXT NOT NULL,
    url             TEXT NOT NULL,
    title           TEXT,
    visit_count     INTEGER NOT NULL,
    last_visited_at INTEGER NOT NULL,
    PRIMARY KEY (workspace, url)
);
