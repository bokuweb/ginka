//! Git-backed checkpoints, three refs per turn.
//!
//! A turn gets three references, not one:
//!
//! * **start** — the working tree exactly as the agent was handed it,
//!   captured before it ran. Uncommitted work included: a file the user edited
//!   in the terminal between turns is part of what the agent was given, not
//!   part of what it did.
//! * **end** — the working tree when the turn settled.
//! * **base** — the commit `HEAD` pointed at when the turn started, so the
//!   turn can also be placed against the branch's history.
//!
//! The turn's own diff is start → end, which is what makes "what did *this
//! turn* change" answerable and keeps a hand edit or a branch switch out of
//! the agent's column. One ref could not tell them apart, and none of this can
//! be reconstructed after the fact — it has to be written while the turn runs.
//! See `docs/roadmap.md` §3.3 N7, N8.

use anyhow::{Context, Result, bail};
use ginka_protocol::ids::slugify;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::git::Git;

/// Identifies one turn of one session. Refs are derived from it, so both parts
/// are slugified into something git will accept as a ref component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnId {
    session: String,
    turn: usize,
}

impl TurnId {
    pub fn new(session: impl AsRef<str>, turn: usize) -> Self {
        Self {
            session: slugify(session.as_ref()),
            turn,
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn turn(&self) -> usize {
        self.turn
    }

    fn ref_for(&self, part: &str) -> String {
        format!(
            "refs/ginka/session/{}/turn/{}/{part}",
            self.session, self.turn
        )
    }

    pub fn start_ref(&self) -> String {
        self.ref_for("start")
    }

    pub fn end_ref(&self) -> String {
        self.ref_for("end")
    }

    pub fn base_ref(&self) -> String {
        self.ref_for("base")
    }
}

/// What was recorded when a turn started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStart {
    /// The tree object holding the working tree as accepted.
    pub tree: String,
    /// The commit wrapping that tree, which the start ref points at.
    pub commit: String,
    /// `HEAD` at the time, or `None` in a repository with no commits yet.
    pub base_commit: Option<String>,
    /// The branch checked out at the time, or `None` when detached.
    pub branch: Option<String>,
}

/// Checkpoints for one worktree.
#[derive(Debug, Clone)]
pub struct Checkpoints {
    git: Git,
    root: PathBuf,
}

impl Checkpoints {
    pub fn new(worktree: impl Into<PathBuf>) -> Self {
        let root = worktree.into();
        Self {
            git: Git::new(root.clone()),
            root,
        }
    }

    pub fn worktree(&self) -> &Path {
        &self.root
    }

    /// Record the state a turn is about to be handed. Capturing twice for the
    /// same turn replaces the earlier record: a retried turn starts from where
    /// the retry started, not from the abandoned attempt.
    pub fn capture_turn_start(&self, turn: &TurnId) -> Result<TurnStart> {
        let base_commit = self.head_commit()?;
        let branch = self.current_branch()?;
        let tree = self.snapshot_tree()?;
        let commit = self.commit_tree(&tree, base_commit.as_deref(), "turn start")?;

        self.set_ref(&turn.start_ref(), &commit)?;
        if let Some(base) = &base_commit {
            self.set_ref(&turn.base_ref(), base)?;
        }
        // A retry must not leave the previous attempt's ending state behind,
        // or the turn's diff would be measured against work that was undone.
        self.delete_ref(&turn.end_ref())?;

        Ok(TurnStart {
            tree,
            commit,
            base_commit,
            branch,
        })
    }

    /// Record the state a turn settled at.
    pub fn capture_turn_end(&self, turn: &TurnId) -> Result<String> {
        let tree = self.snapshot_tree()?;
        let parent = self.resolve(&turn.start_ref())?;
        let commit = self.commit_tree(&tree, parent.as_deref(), "turn end")?;
        self.set_ref(&turn.end_ref(), &commit)?;
        Ok(commit)
    }

    /// Whether this turn was ever checkpointed.
    pub fn exists(&self, turn: &TurnId) -> Result<bool> {
        Ok(self.resolve(&turn.start_ref())?.is_some())
    }

    /// The paths this turn changed — the agent's own work, and nothing that
    /// was already there when it started.
    pub fn changed_files(&self, turn: &TurnId) -> Result<Vec<String>> {
        let Some((start, end)) = self.turn_range(turn)? else {
            return Ok(Vec::new());
        };
        let output = self
            .git
            .run(&["diff", "--name-only", &start, &end])
            .context("listing the files a turn changed")?;
        Ok(output.lines().map(str::to_string).collect())
    }

    /// The patch for this turn, in the same shape the review surface renders.
    pub fn turn_patch(&self, turn: &TurnId) -> Result<String> {
        let Some((start, end)) = self.turn_range(turn)? else {
            return Ok(String::new());
        };
        self.git
            .run(&["diff", &start, &end])
            .context("reading a turn's patch")
    }

    /// Put the working tree back to the state the turn was handed.
    ///
    /// The branch is deliberately untouched: rewinding a conversation is not a
    /// history rewrite, and a user who has committed since must keep those
    /// commits. Ignored files (`.env`, build output) are left alone too.
    pub fn rewind_to_turn_start(&self, turn: &TurnId) -> Result<()> {
        let Some(start) = self.resolve(&turn.start_ref())? else {
            bail!(
                "turn {} of session {} was never checkpointed",
                turn.turn(),
                turn.session()
            );
        };
        // Restore tracked content, then remove what the agent added on top.
        self.git.run(&["read-tree", "--reset", "-u", &start])?;
        self.git.run(&["clean", "-q", "-fd"])?;
        // Leave the index describing HEAD again, so the restored differences
        // read as the unstaged work they were before the turn.
        self.git.run(&["reset", "-q"])?;
        Ok(())
    }

    /// Drop every ref belonging to one session. Called when a session is
    /// deleted, so a repository does not accumulate refs forever.
    pub fn forget_session(&self, session: &str) -> Result<()> {
        let prefix = format!("refs/ginka/session/{}/", slugify(session));
        let listing = self.git.run(&[
            "for-each-ref",
            "--format=%(refname)",
            &format!("{prefix}**"),
        ])?;
        for name in listing.lines() {
            self.delete_ref(name)?;
        }
        Ok(())
    }

    fn turn_range(&self, turn: &TurnId) -> Result<Option<(String, String)>> {
        let (Some(start), Some(end)) = (
            self.resolve(&turn.start_ref())?,
            self.resolve(&turn.end_ref())?,
        ) else {
            return Ok(None);
        };
        Ok(Some((start, end)))
    }

    /// Snapshot the working tree without touching the user's own index.
    ///
    /// The index git stages into is a temporary file, so a checkpoint taken
    /// mid-turn cannot disturb a `git add` the user is in the middle of.
    fn snapshot_tree(&self) -> Result<String> {
        let index = self.root.join(".git").join("ginka-checkpoint-index");
        // A stale index from a killed process would be reused as a starting
        // point and hide deletions, so it is always started from empty.
        if index.exists() {
            std::fs::remove_file(&index)
                .with_context(|| format!("clearing {}", index.display()))?;
        }
        let env = [("GIT_INDEX_FILE", index.as_os_str())];
        let result = (|| -> Result<String> {
            self.git.run_with_env(&["add", "-A", "."], &env)?;
            self.git.run_with_env(&["write-tree"], &env)
        })();
        let _ = std::fs::remove_file(&index);
        result.context("snapshotting the working tree")
    }

    fn commit_tree(&self, tree: &str, parent: Option<&str>, message: &str) -> Result<String> {
        let mut args: Vec<&OsStr> = vec![OsStr::new("commit-tree"), OsStr::new(tree)];
        if let Some(parent) = parent {
            args.push(OsStr::new("-p"));
            args.push(OsStr::new(parent));
        }
        args.push(OsStr::new("-m"));
        args.push(OsStr::new(message));

        // A checkpoint is machinery, not authorship, and it must succeed on a
        // machine where the user has never configured a git identity.
        let identity: [(&str, &OsStr); 4] = [
            ("GIT_AUTHOR_NAME", OsStr::new("Ginka")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("checkpoints@ginka.invalid")),
            ("GIT_COMMITTER_NAME", OsStr::new("Ginka")),
            (
                "GIT_COMMITTER_EMAIL",
                OsStr::new("checkpoints@ginka.invalid"),
            ),
        ];
        self.git.run_with_env(&args, &identity)
    }

    fn head_commit(&self) -> Result<Option<String>> {
        // An unborn HEAD is the first turn in a fresh repository, not an error.
        self.git
            .query(&["rev-parse", "--verify", "--quiet", "HEAD"])
    }

    fn current_branch(&self) -> Result<Option<String>> {
        let branch = self.git.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok((branch != "HEAD").then_some(branch))
    }

    fn resolve(&self, reference: &str) -> Result<Option<String>> {
        self.git
            .query(&["rev-parse", "--verify", "--quiet", reference])
    }

    fn set_ref(&self, name: &str, commit: &str) -> Result<()> {
        self.git.run(&["update-ref", name, commit])?;
        Ok(())
    }

    fn delete_ref(&self, name: &str) -> Result<()> {
        if self.resolve(name)?.is_some() {
            self.git.run(&["update-ref", "-d", name])?;
        }
        Ok(())
    }
}
