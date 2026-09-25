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
use std::ffi::OsStr;
use std::io::Write as _;

/// Re-exported so callers can read a status without naming the protocol crate;
/// it is a wire type because the daemon pushes it to every client.
pub use ginka_protocol::model::BranchStatus;
pub use ginka_protocol::model::MergeOutcome;
use ginka_protocol::model::{ChangeSource, FileChange, GitCommit};

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// Run git without trimming stdout, for patch text whose final whitespace is data.
fn git_raw(repo: &Path, args: &[&str]) -> Result<String> {
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
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run git with text on stdin, for operations such as applying one hunk.
fn git_with_input(repo: &Path, args: &[&str], input: &str) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    child
        .stdin
        .take()
        .context("git stdin was not piped")?
        .write_all(input.as_bytes())
        .context("writing a patch to git")?;
    let output = child
        .wait_with_output()
        .with_context(|| format!("waiting for git {}", args.join(" ")))?;
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

/// Every local branch, with where each is checked out.
///
/// Asked of git rather than kept, because a branch made in a terminal is as
/// real as one made here. `%(worktreepath)` is what says a branch is held by
/// another worktree, which is the one thing a picker has to grey out.
pub fn list_branches(worktree: &Path) -> Result<Vec<ginka_protocol::model::BranchInfo>> {
    let text = git(
        worktree,
        &[
            "branch",
            "--list",
            "--format=%(refname:short)\t%(HEAD)\t%(worktreepath)",
        ],
    )?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let head = parts.next().unwrap_or_default().trim();
            let held = parts.next().unwrap_or_default().trim();
            Some(ginka_protocol::model::BranchInfo {
                name: name.to_string(),
                current: head == "*",
                checked_out_at: (!held.is_empty()).then(|| PathBuf::from(held)),
            })
        })
        .collect())
}

/// Check `branch` out in `worktree`, cutting it from HEAD with `create`.
///
/// `switch` rather than `checkout`: it refuses to touch files, so a branch
/// name that is also a path cannot turn into a restore.
pub fn checkout(worktree: &Path, branch: &str, create: bool) -> Result<()> {
    if create {
        git(worktree, &["switch", "-c", branch])?;
    } else {
        git(worktree, &["switch", branch])?;
    }
    Ok(())
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
///
/// git asks for `--force` twice to remove a locked worktree — once for the
/// changes, once for the lock — so a forced removal of a locked one says it
/// twice. Unforced, git's refusal already names the lock and its reason.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let locked = force
        && list_worktrees(repo)?
            .iter()
            .any(|worktree| worktree.path == path && worktree.locked);
    let path = path.to_string_lossy().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    if locked {
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
        ChangeSource::Unstaged => {
            git(worktree, &["add", "--intent-to-add", "--all"]).ok();
            let mut args = vec!["diff"];
            args.extend(common);
            git(worktree, &args)?
        }
        ChangeSource::Staged => {
            let mut args = vec!["diff", "--cached"];
            args.extend(common);
            git(worktree, &args)?
        }
        ChangeSource::SinceCheckpoint { .. } => {
            unreachable!("a checkpoint is resolved to a commit before this is called")
        }
        ChangeSource::Commit { commit } => {
            anyhow::ensure!(is_object_name(commit), "{commit:?} is not a commit id");
            // Against the first parent, so a merge reads as what it brought
            // into the branch; a root commit is shown against nothing.
            let mut args = vec!["show", "--format=", "--diff-merges=first-parent"];
            args.extend(common);
            args.push(commit);
            git(worktree, &args)?
        }
    };

    Ok(crate::diff::parse(&patch))
}

/// Move one exact hunk into or out of the index.
///
/// The patch is regenerated from the current repository state and selected by
/// its complete `@@` header. If the view is stale, no other hunk is guessed:
/// the operation fails before `git apply` can touch the index.
pub fn stage_hunk(worktree: &Path, path: &str, header: &str, staged: bool) -> Result<()> {
    if staged {
        // Make a new file visible to `git diff` without staging its contents.
        git(worktree, &["add", "--intent-to-add", "--", path]).ok();
    }
    let mut args = vec!["diff"];
    if !staged {
        args.push("--cached");
    }
    args.extend(["--no-color", "--no-ext-diff", "-U3", "--", path]);
    let patch = git_raw(worktree, &args)?;
    let selected = select_hunk_patch(&patch, header)
        .with_context(|| format!("hunk {header} no longer exists in {path}"))?;
    let mut apply = vec!["apply", "--cached", "--whitespace=nowarn"];
    if !staged {
        apply.push("--reverse");
    }
    apply.push("-");
    git_with_input(worktree, &apply, &selected)?;
    Ok(())
}

/// Discard one exact unstaged hunk while preserving the index.
///
/// The worktree-versus-index patch is regenerated immediately before the
/// reverse apply. A header that disappeared or changed is refused so a stale
/// review cannot discard a neighbouring edit.
pub fn revert_hunk(worktree: &Path, path: &str, header: &str) -> Result<()> {
    // Make a new file visible without moving its contents into the index.
    git(worktree, &["add", "--intent-to-add", "--", path]).ok();
    let patch = git_raw(
        worktree,
        &["diff", "--no-color", "--no-ext-diff", "-U3", "--", path],
    )?;
    let selected = select_hunk_patch(&patch, header)
        .with_context(|| format!("hunk {header} no longer exists in {path}"))?;
    git_with_input(
        worktree,
        &["apply", "--reverse", "--whitespace=nowarn", "-"],
        &selected,
    )?;
    Ok(())
}

/// Keep a diff's file prelude and exactly one hunk.
fn select_hunk_patch(patch: &str, header: &str) -> Option<String> {
    let mut prelude = Vec::new();
    let mut selected = Vec::new();
    let mut before_hunks = true;
    let mut found = false;

    for line in patch.lines() {
        if line.starts_with("@@") {
            before_hunks = false;
            if found {
                break;
            }
            if line == header {
                found = true;
                selected.push(line);
            }
            continue;
        }
        if line.starts_with("diff --git ") && !before_hunks {
            break;
        }
        if before_hunks {
            prelude.push(line);
        } else if found {
            selected.push(line);
        }
    }

    found.then(|| {
        prelude
            .into_iter()
            .chain(selected)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    })
}

/// Read recent commits newest first, retaining enough topology for a graph.
///
/// `limit` is capped because this crosses the daemon boundary and the Git
/// surface is a recent-history reader, not an unbounded repository export.
pub fn history(worktree: &Path, limit: usize) -> Result<Vec<GitCommit>> {
    let limit = limit.min(200);
    if limit == 0 || head_commit(worktree).is_none() {
        return Ok(Vec::new());
    }
    let max_count = format!("--max-count={limit}");
    let output = git(
        worktree,
        &[
            "log",
            &max_count,
            "--no-color",
            "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s%x1e",
        ],
    )?;
    output
        .split('\x1e')
        .filter_map(|record| {
            let record = record.trim();
            (!record.is_empty()).then_some(record)
        })
        .map(|record| {
            let mut fields = record.splitn(5, '\x1f');
            let id = fields.next().unwrap_or_default().to_string();
            let parents = fields
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let author = fields.next().unwrap_or_default().to_string();
            let authored_at = fields
                .next()
                .unwrap_or_default()
                .parse::<i64>()
                .context("parsing git commit author time")?;
            let summary = fields.next().unwrap_or_default().to_string();
            if id.is_empty() {
                bail!("git log returned a commit without an object id");
            }
            Ok(GitCommit {
                id,
                parents,
                author,
                authored_at,
                summary,
            })
        })
        .collect()
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

/// Whether `text` is a hexadecimal object name, abbreviated or not.
///
/// The only shape a client may name a commit in. A ref or a revision
/// expression would be read by git as something else — `--output=…` as an
/// option, `HEAD~3..` as a range — and a commit id is all the history ever
/// hands back anyway.
pub fn is_object_name(text: &str) -> bool {
    (4..=64).contains(&text.len()) && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Push the branch and open a pull request for it with the GitHub CLI.
///
/// Answers with the pull request's address. A branch that already has one
/// open is not a failure — the reader wanted to get to it, and `gh` says where
/// it is in its refusal — so that address is the answer too.
pub fn create_pull_request(worktree: &Path, draft: bool) -> Result<String> {
    push(worktree)?;
    let mut command = Command::new("gh");
    crate::tool_path::apply(&mut command);
    command
        .current_dir(worktree)
        .args(["pr", "create", "--fill"]);
    if draft {
        command.arg("--draft");
    }
    let output = command
        .output()
        .context("running gh: the GitHub CLI is what opens a pull request")?;
    let said = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(url) = pull_request_url(&said) {
        return Ok(url);
    }
    bail!("gh pr create failed: {}", complaint(&output))
}

/// The pull request address in what `gh` printed, if there is one.
pub fn pull_request_url(said: &str) -> Option<String> {
    said.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_graphic() || c == '"'))
        .find(|word| word.starts_with("https://") && word.contains("/pull/"))
        .map(str::to_string)
}

/// Update a clean tracked branch without creating a merge commit or rebasing.
///
/// Dirty work is refused before contacting the remote. A missing upstream or
/// divergence is an error for the caller to explain rather than a reason to
/// guess which history the user wants rewritten.
pub fn pull_fast_forward(worktree: &Path) -> Result<String> {
    let branch = current_branch(worktree).context("a detached HEAD has no branch to pull")?;
    if branch_status(worktree)?.dirty {
        anyhow::bail!("branch {branch} has uncommitted work; commit or stash it before pulling");
    }
    let upstream = git(
        worktree,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .with_context(|| format!("branch {branch} has no upstream to pull"))?;
    let remote_key = format!("branch.{branch}.remote");
    let remote = git(worktree, &["config", "--get", &remote_key])
        .with_context(|| format!("branch {branch} has no remote to pull"))?;
    git(worktree, &["fetch", "--prune", remote.trim()])?;
    git(worktree, &["merge", "--ff-only", upstream.trim()])
}

/// Merge `branch` into `into` — fan-out's "merge the winner".
///
/// The merge happens in the worktree that has `into` checked out, so the
/// reader sees the result there rather than a ref that moved under them; it
/// must be clean, because a merge over uncommitted work mixes the two. A fast
/// forward is preferred, then a merge commit. A conflict is aborted and named:
/// resolving it is a decision, not something to leave half-done in someone's
/// checkout. A branch checked out nowhere can only be fast-forwarded, since
/// there is nowhere to make a merge commit.
pub fn merge_into(repo: &Path, branch: &str, into: &str) -> Result<MergeOutcome> {
    for name in [branch, into] {
        if name.starts_with('-') || !branch_exists(repo, name) {
            bail!("there is no branch {name:?}");
        }
    }
    let tip = git(repo, &["rev-parse", &format!("refs/heads/{branch}")])?;
    let target = format!("refs/heads/{into}");
    let checkout = list_worktrees(repo)?
        .into_iter()
        .find(|worktree| worktree.branch.as_deref() == Some(into) && !worktree.prunable);

    let Some(checkout) = checkout else {
        let old = git(repo, &["rev-parse", &target])?;
        if !is_ancestor(repo, &old, &tip) {
            bail!(
                "{into} has moved on from {branch}; check out {into} somewhere to merge it with a merge commit"
            );
        }
        git(repo, &["update-ref", &target, &tip, &old])?;
        return Ok(MergeOutcome {
            into: into.to_string(),
            commit: tip,
            fast_forward: true,
        });
    };

    let path = checkout.path;
    if branch_status(&path)?.dirty {
        bail!(
            "{into} has uncommitted work in {}; commit or stash it before merging",
            path.display()
        );
    }
    let fast_forward = git(&path, &["merge", "--ff-only", "--quiet", &tip]).is_ok();
    if !fast_forward {
        let message = format!("Merge {branch} into {into}");
        if let Err(error) = git(
            &path,
            &["merge", "--no-ff", "--no-edit", "-m", &message, &tip],
        ) {
            let conflicted =
                git(&path, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let _ = git(&path, &["merge", "--abort"]);
            if conflicted.trim().is_empty() {
                return Err(error).context(format!("merging {branch} into {into}"));
            }
            bail!(
                "merging {branch} into {into} conflicts in {}; nothing was changed",
                conflicted.lines().collect::<Vec<_>>().join(", ")
            );
        }
    }
    Ok(MergeOutcome {
        into: into.to_string(),
        commit: head_commit(&path).context("merged, but git reports no HEAD")?,
        fast_forward,
    })
}

/// Whether `ancestor` is reachable from `descendant`.
fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", ancestor, descendant]).is_ok()
}

/// Drop git's records of worktrees whose directories are gone.
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"])?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn only_a_hexadecimal_name_is_taken_as_a_commit() {
        assert!(is_object_name("b6c49339b3b998d228"));
        assert!(is_object_name("ABCDEF12"));
        // An option, a range and a ref are all things git would read as
        // something other than the commit a history row stands for.
        assert!(!is_object_name("--output=/tmp/x"));
        assert!(!is_object_name("HEAD~3"));
        assert!(!is_object_name("main"));
        assert!(!is_object_name("abc"));
    }

    #[test]
    fn the_address_is_found_in_what_gh_said_either_way() {
        assert_eq!(
            pull_request_url("https://github.com/o/r/pull/12\n").as_deref(),
            Some("https://github.com/o/r/pull/12")
        );
        let refused = "a pull request for branch \"x\" into branch \"main\" already exists:\nhttps://github.com/o/r/pull/9";
        assert_eq!(
            pull_request_url(refused).as_deref(),
            Some("https://github.com/o/r/pull/9")
        );
        assert_eq!(pull_request_url("no remote"), None);
    }
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
    fn pulling_fast_forwards_a_clean_tracked_branch() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        git(
            dir.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        )
        .unwrap();
        let local = dir.path().join("local");
        repository(&local);
        git(
            &local,
            &["remote", "add", "upstream", remote.to_str().unwrap()],
        )
        .unwrap();
        git(&local, &["push", "--set-upstream", "upstream", "main"]).unwrap();

        let peer = dir.path().join("peer");
        git(
            dir.path(),
            &["clone", remote.to_str().unwrap(), peer.to_str().unwrap()],
        )
        .unwrap();
        git(&peer, &["config", "user.email", "peer@example.com"]).unwrap();
        git(&peer, &["config", "user.name", "Peer"]).unwrap();
        std::fs::write(peer.join("remote.txt"), "from remote\n").unwrap();
        git(&peer, &["add", "remote.txt"]).unwrap();
        git(&peer, &["commit", "-m", "advance remote"]).unwrap();
        git(&peer, &["push"]).unwrap();

        pull_fast_forward(&local).unwrap();

        assert_eq!(
            std::fs::read_to_string(local.join("remote.txt")).unwrap(),
            "from remote\n"
        );
        assert_eq!(
            git(&local, &["rev-parse", "HEAD"]).unwrap(),
            git(&local, &["rev-parse", "@{upstream}"]).unwrap()
        );
    }

    #[test]
    fn pulling_refuses_dirty_work_before_contacting_the_remote() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "keep this edit\n").unwrap();

        let error = pull_fast_forward(&root).expect_err("dirty work must not be merged over");

        assert!(error.to_string().contains("uncommitted"));
        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "keep this edit\n"
        );
    }

    #[test]
    fn pulling_refuses_diverged_history_without_making_a_merge_commit() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        git(
            dir.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        )
        .unwrap();
        let local = dir.path().join("local");
        repository(&local);
        git(
            &local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .unwrap();
        git(&local, &["push", "--set-upstream", "origin", "main"]).unwrap();
        let peer = dir.path().join("peer");
        git(
            dir.path(),
            &["clone", remote.to_str().unwrap(), peer.to_str().unwrap()],
        )
        .unwrap();
        git(&peer, &["config", "user.email", "peer@example.com"]).unwrap();
        git(&peer, &["config", "user.name", "Peer"]).unwrap();

        std::fs::write(local.join("local.txt"), "local\n").unwrap();
        git(&local, &["add", "local.txt"]).unwrap();
        git(&local, &["commit", "-m", "local commit"]).unwrap();
        let local_head = git(&local, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(peer.join("remote.txt"), "remote\n").unwrap();
        git(&peer, &["add", "remote.txt"]).unwrap();
        git(&peer, &["commit", "-m", "remote commit"]).unwrap();
        git(&peer, &["push"]).unwrap();

        pull_fast_forward(&local).expect_err("divergence must need an explicit user choice");

        assert_eq!(git(&local, &["rev-parse", "HEAD"]).unwrap(), local_head);
        assert!(!local.join("remote.txt").exists());
    }

    #[test]
    fn history_is_newest_first_and_keeps_parent_topology() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        let first = git(&root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("second.txt"), "second\n").unwrap();
        git(&root, &["add", "second.txt"]).unwrap();
        git(&root, &["commit", "-m", "second change"]).unwrap();
        let second = git(&root, &["rev-parse", "HEAD"]).unwrap();

        let commits = history(&root, 20).unwrap();

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].id, second);
        assert_eq!(commits[0].parents.as_slice(), std::slice::from_ref(&first));
        assert_eq!(commits[0].summary, "second change");
        assert_eq!(commits[0].author, "Test");
        assert!(commits[0].authored_at > 0);
        assert_eq!(commits[1].id, first);
        assert!(commits[1].parents.is_empty());
    }

    #[test]
    fn history_is_empty_before_the_first_commit() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--initial-branch=main"]).unwrap();

        assert!(history(dir.path(), 20).unwrap().is_empty());
    }

    #[test]
    fn one_hunk_can_move_between_the_worktree_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        let original = (1..=24)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("tracked.txt"), &original).unwrap();
        git(&root, &["add", "tracked.txt"]).unwrap();
        git(&root, &["commit", "-m", "add enough context"]).unwrap();
        let edited = original
            .replace("line 2\n", "line two\n")
            .replace("line 20\n", "line twenty\n");
        std::fs::write(root.join("tracked.txt"), edited).unwrap();

        let unstaged = changes(&root, &ChangeSource::Unstaged).unwrap();
        assert_eq!(unstaged[0].hunks.len(), 2);
        let first = unstaged[0].hunks[0].header.clone();

        stage_hunk(&root, "tracked.txt", &first, true).unwrap();
        assert_eq!(
            changes(&root, &ChangeSource::Staged).unwrap()[0]
                .hunks
                .len(),
            1
        );
        assert_eq!(
            changes(&root, &ChangeSource::Unstaged).unwrap()[0]
                .hunks
                .len(),
            1
        );

        stage_hunk(&root, "tracked.txt", &first, false).unwrap();
        assert!(changes(&root, &ChangeSource::Staged).unwrap().is_empty());
        assert_eq!(
            changes(&root, &ChangeSource::Unstaged).unwrap()[0]
                .hunks
                .len(),
            2
        );
    }

    #[test]
    fn a_stale_hunk_header_is_refused_without_moving_anything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "changed\n").unwrap();

        let error = stage_hunk(&root, "tracked.txt", "@@ -99 +99 @@ stale", true)
            .expect_err("a stale view must not stage another hunk");

        assert!(error.to_string().contains("no longer exists"));
        assert!(changes(&root, &ChangeSource::Staged).unwrap().is_empty());
    }

    #[test]
    fn one_unstaged_hunk_can_be_discarded_without_touching_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        let original = (1..=30)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("tracked.txt"), &original).unwrap();
        git(&root, &["add", "tracked.txt"]).unwrap();
        git(&root, &["commit", "-m", "add discard fixture"]).unwrap();

        let with_staged_edit = original.replace("line 2\n", "line two\n");
        std::fs::write(root.join("tracked.txt"), &with_staged_edit).unwrap();
        git(&root, &["add", "tracked.txt"]).unwrap();
        let with_both_edits = with_staged_edit.replace("line 29\n", "line twenty-nine\n");
        std::fs::write(root.join("tracked.txt"), with_both_edits).unwrap();
        let header = changes(&root, &ChangeSource::Unstaged).unwrap()[0].hunks[0]
            .header
            .clone();

        revert_hunk(&root, "tracked.txt", &header).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            with_staged_edit
        );
        let staged = changes(&root, &ChangeSource::Staged).unwrap();
        assert_eq!(staged[0].hunks.len(), 1);
        assert!(
            staged[0].hunks[0]
                .lines
                .iter()
                .any(|line| line.text == "line two")
        );
        assert!(changes(&root, &ChangeSource::Unstaged).unwrap().is_empty());
    }

    #[test]
    fn a_stale_discard_header_leaves_the_worktree_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("tracked.txt"), "keep this edit\n").unwrap();

        let error = revert_hunk(&root, "tracked.txt", "@@ -99 +99 @@ stale")
            .expect_err("a stale view must not discard another hunk");

        assert!(error.to_string().contains("no longer exists"));
        assert_eq!(
            std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "keep this edit\n"
        );
    }

    #[test]
    fn discarding_an_untracked_files_only_hunk_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        repository(&root);
        std::fs::write(root.join("new.txt"), "new work\n").unwrap();
        let header = changes(&root, &ChangeSource::Unstaged).unwrap()[0].hunks[0]
            .header
            .clone();

        revert_hunk(&root, "new.txt", &header).unwrap();

        assert!(!root.join("new.txt").exists());
        assert!(changes(&root, &ChangeSource::Unstaged).unwrap().is_empty());
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

/// A `git` invocation bound to one directory.
///
/// The functions above answer a question each and build their own commands.
/// The checkpoint, review and commit paths instead run sequences of plumbing
/// against the same repository — often with a temporary index or a fixed
/// identity in the environment — so they share this runner rather than each
/// spelling out `Command::new("git")`.
#[derive(Debug, Clone)]
pub struct Git {
    cwd: PathBuf,
}

impl Git {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Run git and return its trimmed stdout, failing on a non-zero exit.
    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String> {
        match self.try_run(args, &[])? {
            Outcome::Ok(stdout) => Ok(stdout),
            Outcome::Failed { status, stderr } => {
                bail!("git {} failed ({status}): {stderr}", describe(args))
            }
        }
    }

    /// Run git with extra environment — a temporary index, a fixed identity.
    pub fn run_with_env<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        env: &[(&str, &OsStr)],
    ) -> Result<String> {
        match self.try_run(args, env)? {
            Outcome::Ok(stdout) => Ok(stdout),
            Outcome::Failed { status, stderr } => {
                bail!("git {} failed ({status}): {stderr}", describe(args))
            }
        }
    }

    /// Run git, treating a non-zero exit as an answer rather than an error.
    /// `rev-parse --verify` on a ref that does not exist is a question, not a
    /// failure.
    pub fn query<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Option<String>> {
        Ok(match self.try_run(args, &[])? {
            Outcome::Ok(stdout) => Some(stdout),
            Outcome::Failed { .. } => None,
        })
    }

    /// Run git and hand back its exit code with its output, for the commands
    /// where a non-zero status is part of the answer (`diff --no-index` exits
    /// 1 when it finds a difference).
    pub fn run_lenient<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<(i32, String)> {
        Ok(match self.try_run(args, &[])? {
            Outcome::Ok(stdout) => (0, stdout),
            Outcome::Failed { status, stderr: _ } => (status, String::new()),
        })
    }

    fn try_run<S: AsRef<OsStr>>(&self, args: &[S], env: &[(&str, &OsStr)]) -> Result<Outcome> {
        let mut command = Command::new("git");
        command.args(args).current_dir(&self.cwd);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command
            .output()
            .with_context(|| format!("running git {} in {}", describe(args), self.cwd.display()))?;
        if output.status.success() {
            Ok(Outcome::Ok(
                String::from_utf8_lossy(&output.stdout)
                    .trim_end()
                    .to_string(),
            ))
        } else {
            Ok(Outcome::Failed {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }
}

enum Outcome {
    Ok(String),
    Failed { status: i32, stderr: String },
}

fn describe<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}
