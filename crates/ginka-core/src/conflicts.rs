//! Handing a conflicted worktree to an agent — Orca's "Resolve with AI".
//!
//! A merge, rebase or cherry-pick that stopped on conflicts leaves the
//! worktree in a state git describes precisely: which operation is in
//! progress and which paths are unmerged. That description is the whole
//! prompt; the agent has the files, the markers and git itself. What the
//! prompt adds is the rule a person would otherwise have to type every time:
//! keep both sides' intent, do not abandon the operation, and finish it.

use anyhow::{Context as _, Result};
use std::path::Path;
use std::process::Command;

/// The operation a worktree is part-way through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// `git merge`, stopped with `MERGE_HEAD` set.
    Merge,
    /// `git rebase`, with a `rebase-merge` or `rebase-apply` directory.
    Rebase,
    /// `git cherry-pick`, with `CHERRY_PICK_HEAD` set.
    CherryPick,
    /// `git revert`, with `REVERT_HEAD` set.
    Revert,
}

impl Operation {
    /// The git command that continues it once every path is resolved.
    pub fn continue_command(self) -> &'static str {
        match self {
            Self::Merge => "git commit --no-edit",
            Self::Rebase => "git rebase --continue",
            Self::CherryPick => "git cherry-pick --continue",
            Self::Revert => "git revert --continue",
        }
    }

    /// What the prompt calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

/// Where a worktree stands: the operation in progress, if git has one open,
/// and the paths it could not merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflicts {
    /// What stopped, when git has one open; `None` for unmerged paths with
    /// no operation in progress (a stash pop, say).
    pub operation: Option<Operation>,
    /// The unmerged paths, relative to the worktree root.
    pub paths: Vec<String>,
}

/// Read the worktree's conflicts from git.
pub fn read(worktree: &Path) -> Result<Conflicts> {
    let run = |args: &[&str]| -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        anyhow::ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let paths = run(&["diff", "--name-only", "--diff-filter=U", "-z"])?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
    // A linked worktree's state lives in its own git directory, not in the
    // repository's `.git`.
    let git_dir = std::path::PathBuf::from(run(&["rev-parse", "--absolute-git-dir"])?.trim());
    let operation =
        if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
            Some(Operation::Rebase)
        } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
            Some(Operation::CherryPick)
        } else if git_dir.join("REVERT_HEAD").is_file() {
            Some(Operation::Revert)
        } else if git_dir.join("MERGE_HEAD").is_file() {
            Some(Operation::Merge)
        } else {
            None
        };
    Ok(Conflicts { operation, paths })
}

/// Most paths named in the prompt; the rest are counted. The agent can list
/// them itself, and a prompt the length of a lockfile's diff helps no one.
pub const MAX_PATHS: usize = 50;

/// The message that asks an agent to resolve `conflicts`.
pub fn prompt(conflicts: &Conflicts) -> String {
    let mut text = String::new();
    let what = conflicts
        .operation
        .map(|operation| format!("A {} in this worktree", operation.name()))
        .unwrap_or_else(|| "This worktree".to_string());
    text.push_str(&format!(
        "{what} stopped on conflicts in {} file{}:\n\n",
        conflicts.paths.len(),
        if conflicts.paths.len() == 1 { "" } else { "s" }
    ));
    for path in conflicts.paths.iter().take(MAX_PATHS) {
        text.push_str(&format!("- {path}\n"));
    }
    if conflicts.paths.len() > MAX_PATHS {
        text.push_str(&format!(
            "- …and {} more (`git diff --name-only --diff-filter=U`)\n",
            conflicts.paths.len() - MAX_PATHS
        ));
    }
    text.push_str(
        "\nResolve every conflict. Read both sides and keep what each was for — \
         do not simply take one side. Remove every conflict marker, make sure the \
         result builds and its tests pass, and `git add` each file you resolve.",
    );
    match conflicts.operation {
        Some(operation) => text.push_str(&format!(
            " Then finish the {} with `{}`; do not abort it.",
            operation.name(),
            operation.continue_command()
        )),
        None => text.push_str(" Do not commit."),
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    }

    /// A repository whose `main` and `side` both changed `shared.txt`.
    fn diverged() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@ginka.invalid"],
            &["config", "user.name", "T"],
            &["config", "commit.gpgsign", "false"],
        ] {
            assert!(git(repo, args));
        }
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        assert!(git(repo, &["add", "-A"]));
        assert!(git(repo, &["commit", "-q", "-m", "base"]));
        assert!(git(repo, &["checkout", "-q", "-b", "side"]));
        std::fs::write(repo.join("shared.txt"), "side\n").unwrap();
        assert!(git(repo, &["commit", "-qam", "side"]));
        assert!(git(repo, &["checkout", "-q", "main"]));
        std::fs::write(repo.join("shared.txt"), "main\n").unwrap();
        assert!(git(repo, &["commit", "-qam", "main"]));
        dir
    }

    #[test]
    fn a_stopped_merge_is_read_as_a_merge_with_its_unmerged_paths() {
        let dir = diverged();
        assert!(!git(dir.path(), &["merge", "-q", "side"]), "it conflicts");
        let conflicts = read(dir.path()).unwrap();
        assert_eq!(
            conflicts,
            Conflicts {
                operation: Some(Operation::Merge),
                paths: vec!["shared.txt".into()],
            }
        );
        let text = prompt(&conflicts);
        assert!(text.starts_with("A merge in this worktree stopped on conflicts in 1 file:"));
        assert!(text.contains("- shared.txt"));
        assert!(text.contains("`git commit --no-edit`; do not abort it"));
    }

    #[test]
    fn a_stopped_rebase_is_continued_not_committed() {
        let dir = diverged();
        assert!(!git(dir.path(), &["rebase", "-q", "side"]), "it conflicts");
        let conflicts = read(dir.path()).unwrap();
        assert_eq!(conflicts.operation, Some(Operation::Rebase));
        assert!(prompt(&conflicts).contains("`git rebase --continue`"));
    }

    #[test]
    fn a_clean_worktree_has_nothing_to_resolve() {
        let dir = diverged();
        let conflicts = read(dir.path()).unwrap();
        assert!(conflicts.paths.is_empty());
        assert_eq!(conflicts.operation, None);
    }

    #[test]
    fn a_long_list_is_cut_and_counted() {
        let conflicts = Conflicts {
            operation: None,
            paths: (0..MAX_PATHS + 7).map(|i| format!("f{i}")).collect(),
        };
        let text = prompt(&conflicts);
        assert!(text.contains(&format!("- f{}", MAX_PATHS - 1)));
        assert!(!text.contains(&format!("- f{MAX_PATHS}\n")));
        assert!(text.contains("…and 7 more"));
        assert!(text.contains("Do not commit."));
    }
}
