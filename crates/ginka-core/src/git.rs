//! Git operations, run through the `git` binary.
//!
//! Worktree plumbing has to go through the real binary anyway — it is the only
//! implementation that agrees with the user's own `git worktree list`, hooks
//! and config — so reads go the same way rather than splitting the truth
//! between two implementations. See `docs/roadmap.md` §4.3.
//!
//! Every function here takes a repository path and is free of app state, so it
//! can be exercised against a throwaway repository in a temporary directory.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktree {
    pub path: PathBuf,
    /// The checked-out commit, absent for a worktree that has never had one.
    pub head: Option<String>,
    /// The checked-out branch. `None` means a detached HEAD.
    pub branch: Option<String>,
    /// A locked worktree cannot be removed without `--force`; usually it lives
    /// on removable media or is deliberately pinned.
    pub locked: bool,
    /// The worktree directory is gone from disk but git still tracks it.
    pub prunable: bool,
}

impl GitWorktree {
    /// What to record as the workspace's branch.
    ///
    /// A detached HEAD has no branch name, and the UI needs something stable to
    /// show, so it falls back to the literal git uses for the same situation.
    pub fn branch_label(&self) -> String {
        self.branch.clone().unwrap_or_else(|| "HEAD".to_string())
    }
}

/// Run `git` in `repo` and return stdout, trimmed.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;

    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether `path` is inside a git working tree.
pub fn is_repository(path: &Path) -> bool {
    git(path, &["rev-parse", "--is-inside-work-tree"])
        .map(|out| out == "true")
        .unwrap_or(false)
}

/// The repository's top level, which is what we register rather than whatever
/// subdirectory the user happened to point at.
pub fn top_level(path: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?))
}

pub fn has_origin(repo: &Path) -> bool {
    git(repo, &["remote"])
        .map(|out| out.lines().any(|line| line.trim() == "origin"))
        .unwrap_or(false)
}

/// The branch a new worktree should be cut from.
///
/// Prefers what the remote calls its default, because that is what the user's
/// pull requests target. Falls back to the checked-out branch for a repository
/// with no origin, and finally to `main`.
pub fn default_branch(repo: &Path) -> String {
    if let Ok(reference) = git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(branch) = reference.strip_prefix("origin/")
    {
        return branch.to_string();
    }
    current_branch(repo).unwrap_or_else(|| "main".to_string())
}

/// The branch checked out in `repo`, or `None` when HEAD is detached.
pub fn current_branch(repo: &Path) -> Option<String> {
    let branch = git(repo, &["symbolic-ref", "--short", "HEAD"]).ok()?;
    (!branch.is_empty()).then_some(branch)
}

/// Every worktree git knows about, the main one included.
pub fn list_worktrees(repo: &Path) -> Result<Vec<GitWorktree>> {
    Ok(parse_worktree_list(&git(
        repo,
        &["worktree", "list", "--porcelain"],
    )?))
}

/// Parse `git worktree list --porcelain`.
///
/// Records are separated by blank lines. `worktree` opens a record; `HEAD`,
/// `branch`, `locked`, `prunable` and `bare` are attributes, and `detached`
/// appears instead of `branch`. Attributes may carry a reason after a space
/// (`locked reason`), which we ignore.
fn parse_worktree_list(output: &str) -> Vec<GitWorktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<GitWorktree> = None;

    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            worktrees.extend(current.take());
            continue;
        }

        let (key, value) = match line.split_once(' ') {
            Some((key, value)) => (key, Some(value)),
            None => (line, None),
        };

        match key {
            "worktree" => {
                worktrees.extend(current.take());
                current = Some(GitWorktree {
                    path: PathBuf::from(value.unwrap_or_default()),
                    head: None,
                    branch: None,
                    locked: false,
                    prunable: false,
                });
            }
            "HEAD" => {
                if let Some(worktree) = current.as_mut() {
                    worktree.head = value.map(str::to_string);
                }
            }
            "branch" => {
                if let Some(worktree) = current.as_mut() {
                    // Reported fully qualified: refs/heads/<name>.
                    worktree.branch = value
                        .map(|reference| reference.strip_prefix("refs/heads/").unwrap_or(reference))
                        .map(str::to_string);
                }
            }
            "locked" => {
                if let Some(worktree) = current.as_mut() {
                    worktree.locked = true;
                }
            }
            "prunable" => {
                if let Some(worktree) = current.as_mut() {
                    worktree.prunable = true;
                }
            }
            _ => {}
        }
    }
    worktrees.extend(current);
    worktrees
}

/// Create a worktree at `path` on a new branch cut from `base`.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    let path = path.to_string_lossy().to_string();
    git(repo, &["worktree", "add", "-b", branch, &path, base])?;
    Ok(())
}

/// Remove a worktree. `force` is required for one that is dirty or locked.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let path = path.to_string_lossy().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    git(repo, &args)?;
    Ok(())
}

/// Drop git's records of worktrees whose directories are gone.
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_main_worktree_and_a_linked_one() {
        let output = "\
worktree /home/u/project
HEAD abc123
branch refs/heads/main

worktree /home/u/.ginka/worktrees/project/feature
HEAD def456
branch refs/heads/feature/thing
";
        let parsed = parse_worktree_list(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, PathBuf::from("/home/u/project"));
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert_eq!(parsed[0].head.as_deref(), Some("abc123"));
        // The ref is stripped to a branch name, and a name containing a slash
        // must survive that.
        assert_eq!(parsed[1].branch.as_deref(), Some("feature/thing"));
    }

    #[test]
    fn a_detached_worktree_has_no_branch_but_still_has_a_label() {
        let output = "\
worktree /home/u/project
HEAD abc123
detached
";
        let parsed = parse_worktree_list(output);
        assert_eq!(parsed[0].branch, None);
        assert_eq!(parsed[0].branch_label(), "HEAD");
    }

    #[test]
    fn locked_and_prunable_are_recognised_with_or_without_a_reason() {
        let output = "\
worktree /a
HEAD a1
branch refs/heads/one
locked on a removable drive

worktree /b
HEAD b1
branch refs/heads/two
prunable
";
        let parsed = parse_worktree_list(output);
        assert!(parsed[0].locked, "a reason after `locked` must not hide it");
        assert!(!parsed[0].prunable);
        assert!(parsed[1].prunable);
    }

    #[test]
    fn the_final_record_is_kept_without_a_trailing_blank_line() {
        // git ends its output with a blank line, but a caller that trims the
        // output -- as `git()` does -- removes it.
        let parsed = parse_worktree_list("worktree /only\nHEAD a1\nbranch refs/heads/main");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn empty_output_yields_no_worktrees() {
        assert!(parse_worktree_list("").is_empty());
    }
}
