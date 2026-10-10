//! Guarded turn undo. Only unchanged end states can return to their start.

use super::{Checkpoints, TurnStart};
use crate::git::Git;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
};

/// Durable evidence for undoing one completed turn without rewriting history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoState {
    /// Working tree before the turn, including uncommitted work.
    pub start: String,
    /// Working tree after the turn.
    pub end: String,
    /// Index before the turn, preserving partial staging.
    pub start_index: String,
    /// Index after the turn; later staging must refuse undo too.
    pub end_index: String,
    /// Commit checked out before the turn, absent for an unborn branch.
    pub start_head: Option<String>,
    /// Commit checked out after the turn.
    pub end_head: Option<String>,
    /// Symbolic HEAD before the turn, absent when detached.
    pub start_branch: Option<String>,
    /// Symbolic HEAD after the turn.
    pub end_branch: Option<String>,
}

impl Checkpoints {
    /// Capture an index snapshot without disturbing staging. Special index
    /// flags, sparse checkout, conflicts and intent-to-add are unsupported.
    pub(super) fn capture_index(&self) -> Result<String> {
        self.require_supported_index()?;
        let scratch = ScratchIndex::new(&self.git)?;
        scratch.copy_current(&self.git)?;
        let tree = self.git.run_with_env(
            &["write-tree"],
            &[("GIT_INDEX_FILE", scratch.path.as_os_str())],
        )?;
        self.commit_tree_with_parents(&tree, &[], "turn index")
    }

    /// Record undo evidence for a completed snapshot. Unsupported or changing
    /// states retain their ordinary checkpoint but have no guarded undo.
    pub fn capture_undo(&self, start: &TurnStart, end: &str) -> Result<Option<UndoState>> {
        let Some(start_index) = start.index_commit.clone() else {
            return Ok(None);
        };
        let Some(ignored_paths) = &start.ignored_paths else {
            return Ok(None);
        };
        let Ok(changed) = self.git.run_paths(&[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &start.commit,
            end,
        ]) else {
            return Ok(None);
        };
        // Ignore-rule changes or force-add can expose files the starting tree
        // did not capture. Undo must never mistake them for agent additions.
        if changed
            .iter()
            .any(|path| ignored_paths.iter().any(|root| path.starts_with(root)))
        {
            return Ok(None);
        }
        let end_head = self.head_commit()?;
        let end_branch = self.current_branch()?;
        let snapshot_head =
            self.git
                .query(&["rev-parse", "--verify", "--quiet", &format!("{end}^")])?;
        let Ok(end_index) = self.capture_index() else {
            return Ok(None);
        };
        if snapshot_head != end_head
            || self.snapshot_tree()? != self.tree(end)?
            || self.head_commit()? != end_head
            || self.current_branch()? != end_branch
        {
            return Ok(None);
        }
        Ok(Some(UndoState {
            start: start.commit.clone(),
            end: end.to_string(),
            start_index,
            end_index,
            start_head: start.base_commit.clone(),
            end_head,
            start_branch: start.branch.clone(),
            end_branch,
        }))
    }

    /// Hold all staged and working trees reachable until the checkpoint is pruned.
    pub(super) fn keep_undo(&self, reference: &str, state: &UndoState) -> Result<()> {
        let tree = self.tree(&state.end)?;
        let commit = self.commit_tree_with_parents(
            &tree,
            &[
                &state.start,
                &state.end,
                &state.start_index,
                &state.end_index,
            ],
            "undo evidence",
        )?;
        self.git.run(&["update-ref", reference, &commit])?;
        Ok(())
    }

    /// Undo only if HEAD, branch, index and files still match the completed
    /// turn. The caller must exclude active agents in overlapping directories
    /// and retain a recovery checkpoint first. Ignored files are untouched.
    pub fn undo_turn(&self, state: &UndoState) -> Result<()> {
        let index = index_path(&self.git)?;
        let lock = index.with_extension("lock");
        let handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .context("cannot undo while the Git index is locked")?;
        let guard = IndexLock { path: lock.clone() };
        self.validate_undo(state)?;
        self.refuse_restoration_collisions(state)?;
        // Capturing large working trees can take time. Recheck history at the
        // mutation boundary, too, while the index lock still excludes staging.
        if self.head_commit()? != state.end_head || self.current_branch()? != state.end_branch {
            bail!("cannot undo: Git HEAD or branch changed after validation");
        }
        let scratch = ScratchIndex::new(&self.git)?;
        let env = [("GIT_INDEX_FILE", scratch.path.as_os_str())];
        // The end snapshot tracks even files the agent left untracked. Reading
        // the start against it removes exactly those additions, without a
        // blanket clean of unrelated directories or ignored files.
        self.git.run_with_env(&["read-tree", &state.end], &env)?;
        self.git
            .run_with_env(&["read-tree", "--reset", "-u", &state.start], &env)?;
        self.git
            .run_with_env(&["read-tree", &state.start_index], &env)?;
        fs::copy(&scratch.path, &lock).context("writing restored staging")?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&lock, &index).context("installing restored staging")?;
        drop(guard);
        Ok(())
    }

    fn refuse_restoration_collisions(&self, state: &UndoState) -> Result<()> {
        let added = self.git.run_paths(&[
            "diff",
            "--name-only",
            "--diff-filter=A",
            "--no-renames",
            "-z",
            &state.end,
            &state.start,
        ])?;
        for path in added {
            let destination = self.worktree().join(&path);
            if fs::symlink_metadata(&destination).is_ok() {
                bail!(
                    "cannot undo: an ignored file would be overwritten at {}",
                    path.display()
                );
            }
            let mut parent = destination.parent();
            while let Some(directory) = parent.filter(|directory| *directory != self.worktree()) {
                if let Ok(metadata) = fs::symlink_metadata(directory)
                    && !metadata.is_dir()
                {
                    bail!(
                        "cannot undo: an ignored path would be overwritten at {}",
                        path.display()
                    );
                }
                parent = directory.parent();
            }
        }
        Ok(())
    }

    fn validate_undo(&self, state: &UndoState) -> Result<()> {
        if state.start_head != state.end_head
            || state.start_branch != state.end_branch
            || self.head_commit()? != state.end_head
            || self.current_branch()? != state.end_branch
        {
            bail!("cannot undo: Git HEAD or branch changed since the turn started");
        }
        self.require_supported_index()?;
        if self.tree(&self.capture_index()?)? != self.tree(&state.end_index)? {
            bail!("cannot undo: staging changed after this turn");
        }
        if self.snapshot_tree()? != self.tree(&state.end)? {
            bail!("cannot undo: files changed after this turn");
        }
        for tree in [
            &state.start,
            &state.end,
            &state.start_index,
            &state.end_index,
        ] {
            if self
                .git
                .run(&["ls-tree", "-r", tree])?
                .lines()
                .any(|line| line.starts_with("160000 "))
            {
                bail!("cannot undo a turn containing submodules or nested repositories");
            }
        }
        Ok(())
    }

    fn require_supported_index(&self) -> Result<()> {
        let flags = self.git.run(&["ls-files", "-v", "-z"])?;
        if flags
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .any(|entry| !entry.starts_with("H "))
        {
            bail!("cannot undo an index with conflicts, assume-unchanged or sparse entries");
        }
        let visible =
            self.git
                .run(&["diff", "--cached", "--name-only", "--ita-visible-in-index"])?;
        let invisible = self.git.run(&[
            "diff",
            "--cached",
            "--name-only",
            "--ita-invisible-in-index",
        ])?;
        if visible != invisible {
            bail!("cannot undo an index with intent-to-add entries");
        }
        for name in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ] {
            let path =
                self.git
                    .run(&["rev-parse", "--path-format=absolute", "--git-path", name])?;
            if Path::new(&path).exists() {
                bail!("cannot undo during a Git merge, rebase or cherry-pick");
            }
        }
        Ok(())
    }

    fn tree(&self, commit: &str) -> Result<String> {
        self.git.run(&["rev-parse", &format!("{commit}^{{tree}}")])
    }
}

fn index_path(git: &Git) -> Result<PathBuf> {
    Ok(PathBuf::from(git.run(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ])?))
}

struct IndexLock {
    path: PathBuf,
}
impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A unique private index on the real index's filesystem, removed on drop.
pub(super) struct ScratchIndex {
    /// Path passed to Git through `GIT_INDEX_FILE`.
    pub(super) path: PathBuf,
    _dir: tempfile::TempDir,
}
impl ScratchIndex {
    /// Allocate a private directory without creating an invalid empty index.
    pub(super) fn new(git: &Git) -> Result<Self> {
        let index = index_path(git)?;
        let dir = tempfile::tempdir_in(index.parent().context("index has no parent directory")?)?;
        Ok(Self {
            path: dir.path().join("index"),
            _dir: dir,
        })
    }
    /// Seed from the real index, including entries force-added despite ignores.
    pub(super) fn copy_current(&self, git: &Git) -> Result<()> {
        let index = index_path(git)?;
        if index.exists() {
            fs::copy(index, &self.path)?;
        }
        Ok(())
    }
}

/// Whether two daemon-host workspace roots can affect each other's files.
/// Canonical paths resolve aliases; components keep sibling prefixes separate.
pub fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = canonical_root(left);
    let right = canonical_root(right);
    #[cfg(windows)]
    let (left, right) = (
        PathBuf::from(left.to_string_lossy().to_lowercase()),
        PathBuf::from(right.to_string_lossy().to_lowercase()),
    );
    left.starts_with(&right) || right.starts_with(&left)
}

// Resolve the existing ancestor too: /var and /private/var must still match
// when a descendant has not yet been created.
fn canonical_root(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical_root(parent).join(name),
        _ => path.to_path_buf(),
    }
}
