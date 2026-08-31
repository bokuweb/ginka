-- M1's data model, created in M0 so the migration runner has something real to
-- run. See docs/roadmap.md §4.4.

CREATE TABLE projects (
    name           TEXT PRIMARY KEY,
    path           TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    label          TEXT,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    -- 'git'   -> worktree per workspace, branches, PR/CI features
    -- 'plain' -> a folder with one implicit workspace and git features off
    kind           TEXT NOT NULL DEFAULT 'git' CHECK (kind IN ('git', 'plain')),
    -- NULL means "not probed yet" and is treated as true by the reader, so the
    -- first CI poll after a cold boot still runs.
    has_origin     INTEGER
) STRICT;

CREATE TABLE worktrees (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    project_name TEXT NOT NULL REFERENCES projects(name) ON DELETE CASCADE,
    -- Immutable workspace identity, set once at creation from the slugified
    -- branch name. The workspace id derives from this, NOT from `branch`, so
    -- an agent switching branches inside the worktree does not re-key
    -- everything that hangs off the workspace.
    name         TEXT NOT NULL,
    -- The live branch checked out in the worktree, reconciled against git on
    -- every sync tick. Diverges from `name` once the branch is switched.
    branch       TEXT NOT NULL,
    path         TEXT NOT NULL,
    head         TEXT,
    pinned       INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE UNIQUE INDEX worktrees_project_name_uq ON worktrees (project_name, name);
CREATE INDEX worktrees_project_idx ON worktrees (project_name);
