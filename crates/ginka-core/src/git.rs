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
use ginka_protocol::model::{ChangeSource, FileChange};

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
            complaint(&output)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// What git said about a failure.
///
/// Usually stderr, but not always: `git commit` with nothing staged explains
/// itself on stdout, and an error whose only message went to the stream we
/// ignored reaches the user as no message at all.
fn complaint(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    // Every line of it, because the useful one is not always the first: a
    // commit with nothing staged says "On branch main" before it says why it
    // did nothing. Bounded, because a failing command can be talkative.
    let said = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if said.chars().count() <= 300 {
        return said;
    }
    said.chars().take(299).collect::<String>() + "…"
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

/// Who a checkpoint is committed as.
///
/// Not the user: these are the app's snapshots, and they should read as such
/// in `git show` rather than as commits the user does not remember making.
const CHECKPOINT_AUTHOR: &str = "Ginka";
const CHECKPOINT_EMAIL: &str = "ginka@localhost";

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

    // A checkpoint is Ginka's commit, not the user's: it is on no branch, it
    // was not asked for by name, and attributing it to whoever happens to be
    // configured would put the app's bookkeeping in their name. Carrying its
    // own identity also means a machine that has never run `git config
    // user.email` — a fresh install, a container, a CI runner — can still
    // rewind, instead of failing with an identity error the user has no reason
    // to connect to a checkpoint.
    let identity = [
        ("GIT_AUTHOR_NAME", CHECKPOINT_AUTHOR),
        ("GIT_AUTHOR_EMAIL", CHECKPOINT_EMAIL),
        ("GIT_COMMITTER_NAME", CHECKPOINT_AUTHOR),
        ("GIT_COMMITTER_EMAIL", CHECKPOINT_EMAIL),
    ];
    let commit = match head_commit(worktree) {
        Some(parent) => git_with_env(
            worktree,
            &["commit-tree", &tree, "-p", &parent, "-m", message],
            &identity,
        )?,
        // An empty repository has nothing to hang the snapshot off.
        None => git_with_env(worktree, &["commit-tree", &tree, "-m", message], &identity)?,
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

/// Everything that has changed in a worktree, against `source`.
///
/// Untracked files are included in the uncommitted view by adding them to the
/// index with `--intent-to-add`, which is how `git diff` is made to see a file
/// it has never met. An agent's brand new file is the change most worth
/// reading, and a review that silently omitted it would be worse than useless.
/// `--intent-to-add` records only the path, so nothing of the user's staging is
/// disturbed.
pub fn changes(worktree: &Path, source: &ChangeSource) -> Result<Vec<FileChange>> {
    // Rename detection and no colour: this is parsed, not printed.
    let common = ["--no-color", "--find-renames", "--no-ext-diff", "-U3"];

    let patch = match source {
        ChangeSource::Uncommitted => {
            // Best-effort: a repository with no commits has nothing to add to,
            // and a failure here only costs the untracked files.
            git(worktree, &["add", "--intent-to-add", "--all"]).ok();
            let mut args = vec!["diff", "HEAD"];
            args.extend(common);
            // A repository with no commits cannot be diffed against HEAD.
            match head_commit(worktree) {
                Some(_) => git(worktree, &args)?,
                None => {
                    let mut args = vec!["diff"];
                    args.extend(common);
                    git(worktree, &args)?
                }
            }
        }
        ChangeSource::Staged => {
            let mut args = vec!["diff", "--cached"];
            args.extend(common);
            git(worktree, &args)?
        }
        ChangeSource::SinceCheckpoint { .. } => {
            unreachable!("a checkpoint is resolved to a commit before this is called")
        }
    };

    Ok(crate::diff::parse(&patch))
}

/// Everything that has changed since `commit`, including untracked files.
///
/// This is what "what did this turn do" means: the checkpoint taken at the end
/// of the turn before it is the thing worth comparing against.
pub fn changes_since(worktree: &Path, commit: &str) -> Result<Vec<FileChange>> {
    git(worktree, &["add", "--intent-to-add", "--all"]).ok();
    let patch = git(
        worktree,
        &[
            "diff",
            commit,
            "--no-color",
            "--find-renames",
            "--no-ext-diff",
            "-U3",
        ],
    )?;
    Ok(crate::diff::parse(&patch))
}

/// Lines in the worktree's files that contain `query`.
///
/// `git grep` rather than a walk: it searches the files git knows about,
/// honours the ignore rules, and skips binaries, which is the same set the
/// file finder offers. Untracked files are included — a file the agent wrote a
/// minute ago is exactly the one being searched for.
///
/// A query that matches nothing is not a failure: `git grep` exits 1 to say
/// so, which is the answer rather than an error.
pub fn grep(worktree: &Path, query: &str, limit: usize) -> Result<Vec<(String, u32, String)>> {
    let max = limit.to_string();
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "grep",
            "--line-number",
            "--no-color",
            "--fixed-strings",
            "--ignore-case",
            "--untracked",
            "--max-count",
            &max,
            "-e",
            query,
        ])
        .output()
        .context("running git grep")?;
    // 1 is "nothing matched"; anything else is a real failure.
    if !output.status.success() && output.status.code() != Some(1) {
        bail!(
            "git grep failed in {}: {}",
            worktree.display(),
            complaint(&output)
        );
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(parse_grep_line)
        .take(limit)
        .collect())
}

/// Split `path:line:text`, which is what `git grep --line-number` prints.
///
/// A path can contain a colon and so can the text, so this splits twice from
/// the left and no further: the line number is the first field that parses as
/// one after a path that exists in the output's own shape.
fn parse_grep_line(line: &str) -> Option<(String, u32, String)> {
    let mut at = 0;
    while let Some(colon) = line[at..].find(':') {
        let split = at + colon;
        let rest = &line[split + 1..];
        let second = rest.find(':')?;
        if let Ok(number) = rest[..second].parse::<u32>() {
            return Some((
                line[..split].to_string(),
                number,
                rest[second + 1..].to_string(),
            ));
        }
        at = split + 1;
    }
    None
}

/// Every file git would show in the worktree, tracked or not.
///
/// `--exclude-standard` keeps the ignore rules, which is the difference
/// between offering the user their own source files and offering them
/// `node_modules`.
pub fn ls_files(worktree: &Path) -> Result<Vec<String>> {
    let listed = git(
        worktree,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )?;
    Ok(listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Commit what is in the worktree.
///
/// `all` stages everything first, including files git has never seen, which is
/// what a review that just showed those files leads the user to expect. Without
/// it only what is already staged is committed.
///
/// The commit is the *user's*, so it takes their identity — unlike a
/// checkpoint, which is the app's. A machine with no identity configured fails
/// here with git's own message, which is the right one: this is a commit that
/// will carry their name in a history other people read.
pub fn commit(worktree: &Path, message: &str, all: bool) -> Result<String> {
    if all {
        git(worktree, &["add", "-A"])?;
    }
    git(worktree, &["commit", "-m", message])?;
    head_commit(worktree).context("committed, but git reports no HEAD")
}

/// Stage one path for the next commit.
///
/// Per file rather than all-or-nothing, because a review that ends in "these
/// three files are right and that one is not" has nowhere to put that answer
/// otherwise.
pub fn stage(worktree: &Path, path: &str) -> Result<()> {
    git(worktree, &["add", "--", path])?;
    Ok(())
}

/// Take one path back out of the next commit, leaving the file alone.
///
/// `restore --staged` needs a commit to restore the index entry from, so a
/// repository whose first commit has not happened yet drops the entry instead.
/// That is the same outcome — the file goes back to being untracked — reached
/// the only way git offers before there is a HEAD.
pub fn unstage(worktree: &Path, path: &str) -> Result<()> {
    match head_commit(worktree) {
        Some(_) => git(worktree, &["restore", "--staged", "--", path])?,
        None => git(
            worktree,
            &["rm", "--cached", "--force", "--quiet", "--", path],
        )?,
    };
    Ok(())
}

/// Throw away a file's uncommitted work, staged or not.
///
/// A file that HEAD has never heard of cannot be restored from it — there is
/// nothing to restore — so it is removed from the index and deleted, which is
/// what "undo this file" means for a file the agent created. Destructive by
/// definition: the caller is the one that has to have asked.
pub fn revert_file(worktree: &Path, path: &str) -> Result<()> {
    let known = head_commit(worktree).is_some()
        && git(worktree, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok();
    if known {
        git(worktree, &["restore", "--staged", "--worktree", "--", path])?;
        return Ok(());
    }
    // Best-effort: the file may never have reached the index, and a path git
    // does not know is one there is nothing to remove.
    git(
        worktree,
        &["rm", "--cached", "--force", "--quiet", "--", path],
    )
    .ok();
    let full = worktree.join(path);
    if full.exists() {
        std::fs::remove_file(&full).with_context(|| format!("deleting {path}"))?;
    }
    Ok(())
}

/// Push the worktree's branch, setting an upstream if it has none.
///
/// A branch cut for a workspace has never been pushed, so the first push is
/// always the one that needs `--set-upstream`; making the caller know that is
/// making them do git's bookkeeping.
pub fn push(worktree: &Path) -> Result<String> {
    let branch = current_branch(worktree).context("a detached HEAD has no branch to push")?;
    let status = branch_status(worktree)?;
    if status.untracked_branch {
        git(worktree, &["push", "--set-upstream", "origin", &branch])
    } else {
        git(worktree, &["push"])
    }
}

/// Drop git's records of worktrees whose directories are gone.
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"])?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ginka_protocol::model::ChangeKind;

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
    pub fn repository(root: &Path) {
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
    fn a_repository_with_no_commits_or_identity_can_still_be_snapshotted() {
        // Two things at once, both real: the first thing an agent does in a
        // fresh worktree may be its own first commit, so there has to be
        // something to rewind to before that — and this repository has no
        // `user.email`, as a fresh machine, a container or a CI runner does
        // not. A checkpoint that needed the user's identity would fail there
        // with an error they have no reason to connect to a rewind.
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
    fn listing_files_shows_the_users_own_and_not_what_they_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules/lib.js"), "noise\n").unwrap();
        std::fs::write(root.join(".gitignore"), ".env\nnode_modules\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        std::fs::write(root.join("fresh.rs"), "fn new() {}\n").unwrap();

        let files = ls_files(&root).unwrap();
        assert!(files.contains(&"tracked.txt".to_string()), "{files:?}");
        assert!(
            files.contains(&"fresh.rs".to_string()),
            "a file written a minute ago is the one being reached for: {files:?}"
        );
        assert!(
            !files.iter().any(|path| path.contains("node_modules")),
            "{files:?}"
        );
        assert!(!files.contains(&".env".to_string()), "{files:?}");
    }

    #[test]
    fn committing_takes_everything_the_review_showed() {
        // A review that listed an untracked file and then committed without it
        // would be lying about what it just showed.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(root.join("added.rs"), "fn new() {}\n").unwrap();

        let sha = commit(&root, "the agent's work", true).unwrap();
        assert_eq!(sha, head_commit(&root).unwrap());
        assert!(
            branch_status(&root).unwrap().is_clean(),
            "nothing is left over"
        );

        let listed = git(&root, &["show", "--name-only", "--format=", &sha]).unwrap();
        assert!(listed.contains("added.rs"), "{listed}");
        assert!(listed.contains("tracked.txt"), "{listed}");
    }

    #[test]
    fn committing_without_staging_takes_only_what_was_staged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("ready.txt"), "staged\n").unwrap();
        git(&root, &["add", "ready.txt"]).unwrap();
        std::fs::write(root.join("tracked.txt"), "not yet\n").unwrap();

        let sha = commit(&root, "only what was ready", false).unwrap();
        let listed = git(&root, &["show", "--name-only", "--format=", &sha]).unwrap();
        assert!(listed.contains("ready.txt"));
        assert!(!listed.contains("tracked.txt"));
        assert!(
            branch_status(&root).unwrap().dirty,
            "the rest is still there"
        );
    }

    #[test]
    fn a_commit_with_nothing_to_commit_says_so_rather_than_succeeding() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        let error = commit(&root, "nothing", true).unwrap_err();
        assert!(
            error.to_string().contains("nothing to commit"),
            "git's own words are the clearest here: {error}"
        );
    }

    #[test]
    fn an_agents_new_file_is_part_of_what_it_changed() {
        // The change most worth reading is the one git has never seen, and a
        // review that silently omitted it would be worse than useless.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("added.rs"), "fn new() {}\n").unwrap();
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();

        let changed = changes(&root, &ChangeSource::Uncommitted).unwrap();
        let paths: Vec<&str> = changed.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"added.rs"), "{paths:?}");
        assert!(paths.contains(&"tracked.txt"), "{paths:?}");

        let new_file = changed.iter().find(|file| file.path == "added.rs").unwrap();
        assert_eq!(new_file.kind, ChangeKind::Added);
        assert_eq!(new_file.added, 1);
        assert!(!new_file.hunks.is_empty(), "its contents are readable");
    }

    #[test]
    fn asking_for_changes_does_not_stage_the_users_work() {
        // Untracked files are made visible with `--intent-to-add`, which
        // records the path and nothing else; a review must not quietly stage a
        // half-finished file for the user's next commit.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();

        changes(&root, &ChangeSource::Uncommitted).unwrap();
        let staged = git(&root, &["diff", "--cached", "--name-only"]).unwrap();
        assert!(
            !staged.contains("tracked.txt"),
            "an edited file must not become staged by being read: {staged}"
        );
    }

    #[test]
    fn staged_and_unstaged_work_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("staged.txt"), "ready\n").unwrap();
        git(&root, &["add", "staged.txt"]).unwrap();
        std::fs::write(root.join("tracked.txt"), "not ready\n").unwrap();

        let staged = changes(&root, &ChangeSource::Staged).unwrap();
        let paths: Vec<&str> = staged.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, vec!["staged.txt"]);
    }

    #[test]
    fn what_happened_since_a_checkpoint_is_readable() {
        // The point of taking one at every turn: "what did this turn do".
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        let before = snapshot(&root, REF, "before the turn").unwrap();

        std::fs::write(root.join("tracked.txt"), "the agent's work\n").unwrap();
        std::fs::write(root.join("agent.rs"), "fn added() {}\n").unwrap();

        let changed = changes_since(&root, &before).unwrap();
        let paths: Vec<&str> = changed.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"tracked.txt"), "{paths:?}");
        assert!(paths.contains(&"agent.rs"), "{paths:?}");
    }

    #[test]
    fn a_repository_with_no_commits_still_reports_its_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("empty");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("first.txt"), "words\n").unwrap();

        let changed = changes(&root, &ChangeSource::Uncommitted).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].path, "first.txt");
    }

    #[test]
    fn a_checkpoint_is_committed_as_the_app_rather_than_as_the_user() {
        // `git show` on a checkpoint should not read as a commit the user does
        // not remember making.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        let commit = snapshot(&root, REF, "checkpoint").unwrap();
        let author = git(&root, &["show", "-s", "--format=%an <%ae>", &commit]).unwrap();
        assert_eq!(author, "Ginka <ginka@localhost>");
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

    #[test]
    fn a_file_can_be_staged_and_taken_back_out_on_its_own() {
        // The point of staging per file: "these two are right, that one is
        // not" has nowhere to go otherwise.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(root.join("other.txt"), "new\n").unwrap();

        stage(&root, "tracked.txt").unwrap();
        let staged = changes(&root, &ChangeSource::Staged).unwrap();
        let paths: Vec<&str> = staged.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["tracked.txt"], "only what was staged is staged");

        unstage(&root, "tracked.txt").unwrap();
        assert!(
            changes(&root, &ChangeSource::Staged).unwrap().is_empty(),
            "unstaging leaves nothing for the next commit"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "edited\n",
            "unstaging is about the index, not the file"
        );
    }

    #[test]
    fn staging_works_before_there_is_anything_to_restore_from() {
        // A repository whose first commit has not happened has no HEAD, and
        // `restore --staged` has nothing to read the entry back from.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("first.txt"), "hello\n").unwrap();

        stage(&root, "first.txt").unwrap();
        assert_eq!(changes(&root, &ChangeSource::Staged).unwrap().len(), 1);
        unstage(&root, "first.txt").unwrap();
        assert!(changes(&root, &ChangeSource::Staged).unwrap().is_empty());
        assert!(
            root.join("first.txt").exists(),
            "the file is not the target"
        );
    }

    #[test]
    fn reverting_a_file_puts_it_back_and_deletes_one_that_was_never_there() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(root.join("invented.rs"), "fn wrong() {}\n").unwrap();
        // What the changes panel does before it draws, which leaves the new
        // file in the index — a revert has to cope with that.
        changes(&root, &ChangeSource::Uncommitted).unwrap();

        revert_file(&root, "tracked.txt").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "original\n"
        );

        revert_file(&root, "invented.rs").unwrap();
        assert!(
            !root.join("invented.rs").exists(),
            "undoing a file the agent invented means the file is gone"
        );
        assert!(
            changes(&root, &ChangeSource::Uncommitted)
                .unwrap()
                .is_empty(),
            "nothing is left over"
        );
    }

    #[test]
    fn reverting_a_staged_file_undoes_the_staging_too() {
        // Half an undo -- the working tree restored, the old edit still queued
        // for the next commit -- is worse than none.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "edited\n").unwrap();
        stage(&root, "tracked.txt").unwrap();

        revert_file(&root, "tracked.txt").unwrap();
        assert!(changes(&root, &ChangeSource::Staged).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn searching_finds_the_line_and_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(
            root.join("tracked.txt"),
            "first line\nthe needle is here\nthird line\n",
        )
        .unwrap();
        // Untracked on purpose: the file the agent wrote a minute ago is
        // exactly the one being searched for.
        std::fs::write(root.join("fresh.rs"), "// needle in a new file\n").unwrap();
        std::fs::write(root.join(".env"), "needle=secret\n").unwrap();

        let found = grep(&root, "needle", 20).unwrap();
        let paths: Vec<&str> = found.iter().map(|(path, _, _)| path.as_str()).collect();
        assert!(paths.contains(&"tracked.txt"), "{found:?}");
        assert!(paths.contains(&"fresh.rs"), "{found:?}");
        assert!(
            !paths.contains(&".env"),
            "an ignored file is not the user's to search: {found:?}"
        );

        let hit = found
            .iter()
            .find(|(path, _, _)| path == "tracked.txt")
            .unwrap();
        assert_eq!(hit.1, 2, "the line number is what makes a hit reachable");
        assert_eq!(hit.2, "the needle is here");
    }

    #[test]
    fn a_search_that_matches_nothing_is_an_answer_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        assert!(
            grep(&root, "no such thing anywhere", 20)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_path_with_a_colon_in_it_still_parses() {
        // Both the path and the matched text can contain colons; the line
        // number is the field between them.
        assert_eq!(
            parse_grep_line("src/a:b.rs:12:let x = a:b;"),
            Some(("src/a:b.rs".to_string(), 12, "let x = a:b;".to_string()))
        );
        assert_eq!(parse_grep_line("nothing useful"), None);
    }
}
