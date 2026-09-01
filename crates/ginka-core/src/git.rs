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

/// Re-exported so callers can read a status without naming the protocol crate;
/// it is a wire type because the daemon pushes it to every client.
pub use ginka_protocol::model::BranchStatus;

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

/// Read the status of the worktree at `path`.
pub fn branch_status(path: &Path) -> Result<BranchStatus> {
    Ok(parse_status(&git(
        path,
        &["status", "--porcelain=v2", "--branch"],
    )?))
}

/// Parse `git status --porcelain=v2 --branch`.
///
/// Header lines start with `#`; `# branch.ab +N -M` carries the divergence and
/// is absent when the branch has no upstream. Entry lines are `1` (ordinary),
/// `2` (renamed), `u` (unmerged) and `?` (untracked); anything but a header
/// means the worktree is dirty, and `u` specifically means a conflict.
fn parse_status(output: &str) -> BranchStatus {
    let mut status = BranchStatus {
        untracked_branch: true,
        ..Default::default()
    };

    for line in output.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            if let Some(counts) = header.strip_prefix("branch.ab ") {
                status.untracked_branch = false;
                for field in counts.split_whitespace() {
                    let (sign, number) = field.split_at(1);
                    let value: u32 = number.parse().unwrap_or(0);
                    match sign {
                        "+" => status.ahead = value,
                        "-" => status.behind = value,
                        _ => {}
                    }
                }
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        status.dirty = true;
        if line.starts_with("u ") {
            status.conflict = true;
        }
    }
    status
}

/// When the worktree's HEAD commit was made, as a Unix timestamp.
///
/// Returned as a number rather than git's own relative string: `%cr` is
/// localized and its wording changes between versions, so formatting it is our
/// job, not git's.
pub fn last_commit_time(path: &Path) -> Option<i64> {
    git(path, &["log", "-1", "--format=%ct"]).ok()?.parse().ok()
}

/// Whether `branch` already exists locally.
pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// Create a worktree at `path` for `branch`, cutting it from `base` if it does
/// not exist yet.
///
/// Removing a workspace deliberately leaves its branch behind — the work on it
/// may still matter — so asking for the same workspace again is a normal thing
/// to do, and it checks the existing branch out instead of failing.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    let path = path.to_string_lossy().to_string();
    if branch_exists(repo, branch) {
        git(repo, &["worktree", "add", &path, branch])?;
    } else {
        git(repo, &["worktree", "add", "-b", branch, &path, base])?;
    }
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

/// Snapshot everything in a worktree, and return the commit holding it.
///
/// The snapshot is taken through a scratch index so the user's own index is
/// untouched, and it is committed onto no branch: it exists only as a ref
/// under `refs/ginka/checkpoints/`, which keeps it out of `git log` and out of
/// reach of garbage collection at the same time. Nothing about the user's
/// history moves, which is the whole point — a checkpoint has to be safe to
/// take in the middle of someone else's work.
///
/// Untracked files are included; ignored ones are not, because that is where
/// `node_modules` and `.env` live.
pub fn snapshot(worktree: &Path, reference: &str, message: &str) -> Result<String> {
    let index = scratch_index(worktree)?;
    let index = index.to_string_lossy().to_string();
    let env = [("GIT_INDEX_FILE", index.as_str())];

    // Seed the scratch index from HEAD when there is one, so files that are
    // unchanged are recorded as they already are rather than re-hashed.
    if head_commit(worktree).is_some() {
        git_with_env(worktree, &["read-tree", "HEAD"], &env)?;
    }
    git_with_env(worktree, &["add", "-A"], &env)?;
    let tree = git_with_env(worktree, &["write-tree"], &env)?;
    std::fs::remove_file(&index).ok();

    let commit = match head_commit(worktree) {
        Some(parent) => git(
            worktree,
            &["commit-tree", &tree, "-p", &parent, "-m", message],
        )?,
        // An empty repository has nothing to hang the snapshot off.
        None => git(worktree, &["commit-tree", &tree, "-m", message])?,
    };
    git(worktree, &["update-ref", reference, &commit])?;
    Ok(commit)
}

/// Put a worktree back to the state a snapshot captured.
///
/// Files the snapshot had are restored, and files created since are removed —
/// a rewind that left new files behind would not be one. Ignored files are
/// left alone, because they were never in the snapshot to restore.
///
/// This is destructive by construction, so callers take a snapshot of the
/// current state first: rewinding must never be the thing that loses work.
pub fn restore_snapshot(worktree: &Path, commit: &str) -> Result<()> {
    git(worktree, &["read-tree", "-u", "--reset", commit])?;
    // Everything in the snapshot is in the index now, so what `clean` can see
    // is exactly what was added afterwards.
    git(worktree, &["clean", "-fd"])?;
    Ok(())
}

/// Delete a checkpoint's ref, letting git collect the commit.
pub fn drop_snapshot(worktree: &Path, reference: &str) -> Result<()> {
    git(worktree, &["update-ref", "-d", reference])?;
    Ok(())
}

/// The commit HEAD points at, or `None` in a repository with no commits.
pub fn head_commit(worktree: &Path) -> Option<String> {
    git(worktree, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .ok()
        .filter(|commit| !commit.is_empty())
}

/// A private index file to build a snapshot in.
///
/// It lives beside the worktree's own index — inside the git directory, which
/// for a linked worktree is its own directory under `.git/worktrees/` — so two
/// workspaces snapshotting at once cannot write over each other.
fn scratch_index(worktree: &Path) -> Result<PathBuf> {
    let git_dir = git(worktree, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(git_dir).join("ginka-snapshot-index"))
}

/// Run `git` with extra environment, and return stdout, trimmed.
fn git_with_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
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
    fn a_clean_tracked_branch_reads_as_clean() {
        let status = parse_status(
            "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
        );
        assert!(status.is_clean());
        assert!(!status.untracked_branch);
    }

    #[test]
    fn divergence_is_read_from_the_branch_ab_header() {
        let status = parse_status("# branch.ab +3 -2\n");
        assert_eq!(status.ahead, 3);
        assert_eq!(status.behind, 2);
        assert!(!status.is_clean());
    }

    #[test]
    fn a_branch_with_no_upstream_is_marked_rather_than_reported_as_level() {
        // Without `branch.ab` there is nothing to compare against; rendering
        // "0 ahead, 0 behind" would claim the branch is in sync when it simply
        // has no upstream.
        let status = parse_status("# branch.oid abc\n# branch.head feature\n");
        assert!(status.untracked_branch);
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn entries_make_the_worktree_dirty_and_unmerged_ones_signal_a_conflict() {
        let modified =
            parse_status("# branch.ab +0 -0\n1 .M N... 100644 100644 100644 a b file.rs\n");
        assert!(modified.dirty);
        assert!(!modified.conflict);

        let untracked = parse_status("# branch.ab +0 -0\n? new-file.rs\n");
        assert!(untracked.dirty, "untracked files count as dirty");

        let unmerged = parse_status(
            "# branch.ab +0 -0\nu UU N... 100644 100644 100644 100644 a b c d file.rs\n",
        );
        assert!(unmerged.conflict);
        assert!(unmerged.dirty);
    }

    #[test]
    fn empty_output_yields_no_worktrees() {
        assert!(parse_worktree_list("").is_empty());
    }

    /// A repository with one commit, `.env` ignored, and no remote.
    fn repository(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git(root, &args).unwrap();
        }
        std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
        std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
        git(root, &["add", "."]).unwrap();
        git(root, &["commit", "-m", "first"]).unwrap();
    }

    const REF: &str = "refs/ginka/checkpoints/test";

    #[test]
    fn a_snapshot_captures_modified_and_untracked_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();

        let commit = snapshot(&root, REF, "checkpoint").unwrap();
        let listed = git(&root, &["ls-tree", "-r", "--name-only", &commit]).unwrap();
        assert!(
            listed.contains("new.txt"),
            "untracked work is part of the state"
        );
        assert_eq!(
            git(&root, &["show", &format!("{commit}:tracked.txt")]).unwrap(),
            "edited"
        );
    }

    #[test]
    fn taking_a_snapshot_leaves_the_branch_and_the_index_alone() {
        // It runs in the middle of someone else's work; moving HEAD or staging
        // their files would be unforgivable.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
        let head_before = head_commit(&root).unwrap();
        let status_before = branch_status(&root).unwrap();

        snapshot(&root, REF, "checkpoint").unwrap();

        assert_eq!(head_commit(&root).unwrap(), head_before);
        assert_eq!(branch_status(&root).unwrap(), status_before);
        assert_eq!(
            git(&root, &["diff", "--cached", "--name-only"]).unwrap(),
            "",
            "nothing was staged"
        );
    }

    #[test]
    fn a_snapshot_is_not_on_any_branch_but_survives_garbage_collection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
        let commit = snapshot(&root, REF, "checkpoint").unwrap();

        assert!(
            !git(&root, &["log", "--oneline", "main"])
                .unwrap()
                .contains(&commit[..7]),
            "checkpoints must not appear in the user's history"
        );
        git(&root, &["gc", "--prune=now", "--quiet"]).unwrap();
        assert!(
            git(&root, &["cat-file", "-e", &commit]).is_ok(),
            "a collected checkpoint is a rewind that cannot happen"
        );
    }

    #[test]
    fn restoring_puts_files_back_and_removes_what_came_after() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "the good version\n").unwrap();
        let commit = snapshot(&root, REF, "checkpoint").unwrap();

        // The agent carries on and makes a mess.
        std::fs::write(root.join("tracked.txt"), "the bad version\n").unwrap();
        std::fs::write(root.join("regret.txt"), "should not survive\n").unwrap();
        std::fs::create_dir_all(root.join("junk")).unwrap();
        std::fs::write(root.join("junk/more.txt"), "nor this\n").unwrap();

        restore_snapshot(&root, &commit).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "the good version\n"
        );
        assert!(!root.join("regret.txt").exists());
        assert!(
            !root.join("junk").exists(),
            "a rewind removes new directories too"
        );
    }

    #[test]
    fn restoring_leaves_ignored_files_alone() {
        // `.env` and `node_modules` were never in the snapshot; deleting them
        // would turn a rewind into a re-setup.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        let commit = snapshot(&root, REF, "checkpoint").unwrap();
        std::fs::write(root.join("tracked.txt"), "changed\n").unwrap();

        restore_snapshot(&root, &commit).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "SECRET=1\n"
        );
    }

    #[test]
    fn a_repository_with_no_commits_can_still_be_snapshotted() {
        // The first thing an agent does in a fresh worktree may be its own
        // first commit; there has to be something to rewind to before that.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("empty");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("draft.txt"), "first words\n").unwrap();

        let commit = snapshot(&root, REF, "checkpoint").unwrap();
        assert!(
            git(&root, &["ls-tree", "-r", "--name-only", &commit])
                .unwrap()
                .contains("draft.txt")
        );
    }

    #[test]
    fn the_scratch_index_does_not_outlive_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        snapshot(&root, REF, "checkpoint").unwrap();
        let git_dir = git(&root, &["rev-parse", "--absolute-git-dir"]).unwrap();
        assert!(!Path::new(&git_dir).join("ginka-snapshot-index").exists());
    }

    #[test]
    fn a_dropped_snapshot_is_no_longer_referenced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        snapshot(&root, REF, "checkpoint").unwrap();
        drop_snapshot(&root, REF).unwrap();
        assert!(git(&root, &["rev-parse", "--verify", "--quiet", REF]).is_err());
    }
}
