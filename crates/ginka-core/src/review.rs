//! Diffs, addressed by source.
//!
//! The review surface never asks "what does `git status` say" — it asks a
//! question with a subject: what did *this turn* change, what is staged, what
//! has this branch done since it forked. `git status` can answer only one of
//! those, and the one it answers is not the one the review loop asks most.
//! See `docs/roadmap.md` §3.3 N7.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

use crate::checkpoint::TurnId;
use crate::git::Git;

/// Git's well-known empty tree, used as the parent of a root commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// How much of an untracked file is inlined into a patch before it is cut. A
/// review is read by a person and sent to an agent; neither wants a megabyte
/// of generated fixture.
const MAX_UNTRACKED_PATCH_BYTES: usize = 512 * 1024;

/// Which change set to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSource {
    /// What one agent turn changed, from its checkpoints.
    Turn(TurnId),
    /// Everything not committed: staged and unstaged together, plus untracked.
    Uncommitted,
    /// Working tree against the index.
    Unstaged,
    /// Index against `HEAD`.
    Staged,
    /// One commit against its parent.
    Committed(String),
    /// Everything this branch has done since it forked from `base`.
    Branch { base: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChanged,
    Unknown,
}

impl ChangeStatus {
    fn parse(code: &str) -> Self {
        match code.chars().next() {
            Some('A') => Self::Added,
            Some('M') => Self::Modified,
            Some('D') => Self::Deleted,
            Some('R') => Self::Renamed,
            Some('T') => Self::TypeChanged,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub status: ChangeStatus,
    /// `None` for a binary file, where lines are not the unit.
    pub insertions: Option<u32>,
    pub deletions: Option<u32>,
    pub is_binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub source: DiffSource,
    /// Sorted by path, so a re-render never reshuffles the list under a
    /// reader who is part-way down it.
    pub files: Vec<FileChange>,
    pub patch: String,
}

impl Review {
    pub fn insertions(&self) -> u32 {
        self.files.iter().filter_map(|file| file.insertions).sum()
    }

    pub fn deletions(&self) -> u32 {
        self.files.iter().filter_map(|file| file.deletions).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Build the change set for one source.
pub fn review(worktree: &Path, source: &DiffSource) -> Result<Review> {
    let git = Git::new(worktree);
    let (revs, include_untracked) = revisions_for(&git, source)?;

    let Some(revs) = revs else {
        // A turn that was never checkpointed has no range to diff, which is an
        // empty answer rather than an error: the session may simply predate
        // checkpointing.
        return Ok(Review {
            source: source.clone(),
            files: Vec::new(),
            patch: String::new(),
        });
    };

    let mut files = tracked_changes(&git, &revs)?;
    let mut patch = tracked_patch(&git, &revs)?;

    if include_untracked {
        for path in untracked_paths(&git)? {
            let (change, file_patch) = untracked_change(worktree, &path)?;
            files.push(change);
            patch.push_str(&file_patch);
        }
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Review {
        source: source.clone(),
        files,
        patch,
    })
}

/// The revision arguments for a source, and whether untracked files count as
/// part of it. `Ok(None)` means "there is nothing to compare".
fn revisions_for(git: &Git, source: &DiffSource) -> Result<(Option<Vec<String>>, bool)> {
    Ok(match source {
        // Working-tree sources: untracked files are part of the answer,
        // because a file the agent created is a change whether or not anyone
        // has told git about it yet.
        DiffSource::Uncommitted => (Some(vec!["HEAD".into()]), true),
        DiffSource::Unstaged => (Some(Vec::new()), true),
        DiffSource::Staged => (Some(vec!["--cached".into()]), false),
        DiffSource::Committed(rev) => {
            let parent = git
                .query(&["rev-parse", "--verify", "--quiet", &format!("{rev}^")])?
                .unwrap_or_else(|| EMPTY_TREE.to_string());
            (Some(vec![parent, rev.clone()]), false)
        }
        DiffSource::Branch { base } => {
            let Some(fork) = git.query(&["merge-base", base, "HEAD"])? else {
                return Ok((None, false));
            };
            (Some(vec![fork]), true)
        }
        DiffSource::Turn(turn) => {
            let start = git.query(&["rev-parse", "--verify", "--quiet", &turn.start_ref()])?;
            let end = git.query(&["rev-parse", "--verify", "--quiet", &turn.end_ref()])?;
            match (start, end) {
                (Some(start), Some(end)) => (Some(vec![start, end]), false),
                _ => (None, false),
            }
        }
    })
}

fn tracked_changes(git: &Git, revs: &[String]) -> Result<Vec<FileChange>> {
    let mut counts: BTreeMap<String, (Option<u32>, Option<u32>)> = BTreeMap::new();
    for line in run_diff(git, revs, &["--numstat"])?.lines() {
        let mut parts = line.split('\t');
        let (Some(insertions), Some(deletions), Some(path)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // Git writes "-" for both counts of a binary file.
        counts.insert(
            path.to_string(),
            (insertions.parse().ok(), deletions.parse().ok()),
        );
    }

    let mut files = Vec::new();
    for line in run_diff(git, revs, &["--name-status"])?.lines() {
        let Some((code, path)) = line.split_once('\t') else {
            continue;
        };
        let path = path.to_string();
        let (insertions, deletions) = counts.remove(&path).unwrap_or((None, None));
        files.push(FileChange {
            path,
            status: ChangeStatus::parse(code),
            insertions,
            deletions,
            is_binary: insertions.is_none() && deletions.is_none(),
        });
    }
    Ok(files)
}

fn tracked_patch(git: &Git, revs: &[String]) -> Result<String> {
    run_diff(git, revs, &[])
}

fn run_diff(git: &Git, revs: &[String], extra: &[&str]) -> Result<String> {
    let mut args: Vec<String> = vec!["diff".into(), "--no-color".into(), "--no-renames".into()];
    args.extend(extra.iter().map(|arg| (*arg).to_string()));
    args.extend(revs.iter().cloned());
    git.run(&args).context("running git diff")
}

fn untracked_paths(git: &Git) -> Result<Vec<String>> {
    // `--exclude-standard` is what keeps `.gitignore`d build output and
    // secrets out of every review.
    let listing = git.run(&["ls-files", "--others", "--exclude-standard"])?;
    Ok(listing.lines().map(str::to_string).collect())
}

/// Untracked files have no counterpart in the index, so git cannot diff them
/// against anything; the patch is synthesised instead of shelling out to
/// `diff --no-index`, which would need a platform-specific null device.
fn untracked_change(worktree: &Path, path: &str) -> Result<(FileChange, String)> {
    let bytes = std::fs::read(worktree.join(path))
        .with_context(|| format!("reading untracked file {path}"))?;
    let is_binary = bytes.contains(&0);
    let header = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n");

    if is_binary {
        return Ok((
            FileChange {
                path: path.to_string(),
                status: ChangeStatus::Added,
                insertions: None,
                deletions: None,
                is_binary: true,
            },
            format!("{header}Binary files /dev/null and b/{path} differ\n"),
        ));
    }

    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let mut body = format!(
        "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    let mut truncated = false;
    for line in &lines {
        if body.len() + line.len() > MAX_UNTRACKED_PATCH_BYTES {
            truncated = true;
            break;
        }
        body.push('+');
        body.push_str(line);
        body.push('\n');
    }
    if truncated {
        body.push_str("+… truncated: the file is too large to show in full\n");
    }

    Ok((
        FileChange {
            path: path.to_string(),
            status: ChangeStatus::Added,
            insertions: Some(lines.len() as u32),
            deletions: Some(0),
            is_binary: false,
        },
        format!("{header}{body}"),
    ))
}
