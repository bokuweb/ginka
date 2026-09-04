//! What a project asks for when a new worktree is made.
//!
//! A fresh worktree is a clean checkout, which means the files a repository
//! deliberately does not track are not in it: `.env`, a local database file, a
//! `node_modules` built by a package manager. An agent started there fails on
//! the first command for a reason that has nothing to do with what it was
//! asked, and the user is the one who has to work that out.
//!
//! So a project can say what to bring across and what to run, in
//! `.ginka/config.json` at its root. It is a per-project opt-in and it is
//! documented as one (`docs/roadmap.md` §6.3): copying untracked files between
//! directories is exactly the kind of thing that should never be a default.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Where a project keeps what it wants done for a new worktree.
pub const CONFIG: &str = ".ginka/config.json";

/// How long one setup command is given.
///
/// An install can be slow, but a command that hangs would hold the request
/// that created the workspace open forever, and the workspace itself already
/// exists by then.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// A project's setup, as `.ginka/config.json` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Setup {
    /// Paths, relative to the project root, to copy into a new worktree.
    ///
    /// Directories are copied whole. A path that is not there is skipped: a
    /// developer without a `.env` is not a broken project.
    pub copy: Vec<String>,
    /// Shell commands to run in the new worktree, in order.
    pub commands: Vec<String>,
}

/// Read a project's setup. A project with no file asks for nothing.
///
/// A file that does not parse is reported rather than ignored: it was written
/// on purpose, and silently doing nothing with it is how a user ends up
/// debugging a worktree instead of a typo.
pub fn read(project: &Path) -> Result<Setup> {
    let path = project.join(CONFIG);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Setup::default());
    };
    serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))
}

/// What running a project's setup did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The paths that were brought across.
    pub copied: Vec<String>,
    /// The commands that were run, and whether each succeeded.
    pub ran: Vec<(String, bool)>,
    /// What went wrong, for a log. A worktree that exists is worth keeping
    /// even when its setup did not finish.
    pub problems: Vec<String>,
}

/// Bring across what the project asked for, then run what it asked for.
///
/// Never fails as a whole: the worktree exists by the time this runs, and
/// removing it because an install failed would be throwing away the branch the
/// user just made. What went wrong is collected and reported.
pub fn run(project: &Path, worktree: &Path, setup: &Setup) -> Report {
    let mut report = Report::default();
    for path in &setup.copy {
        // A path that climbs out of the project is not the project's to copy,
        // and this file is in the repository, which means it can arrive in a
        // pull request from anyone.
        if path.split(['/', '\\']).any(|part| part == "..") || Path::new(path).is_absolute() {
            report
                .problems
                .push(format!("{path} is outside the project"));
            continue;
        }
        let from = project.join(path);
        if !from.exists() {
            continue;
        }
        let to = worktree.join(path);
        match copy(&from, &to) {
            Ok(()) => report.copied.push(path.clone()),
            Err(error) => report.problems.push(format!("copying {path}: {error}")),
        }
    }

    for command in &setup.commands {
        match run_command(worktree, command) {
            Ok(true) => report.ran.push((command.clone(), true)),
            Ok(false) => {
                report.ran.push((command.clone(), false));
                report.problems.push(format!("`{command}` failed"));
            }
            Err(error) => {
                report.ran.push((command.clone(), false));
                report.problems.push(format!("`{command}`: {error}"));
            }
        }
    }
    report
}

/// Copy a file, or a directory and everything under it.
fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(from, to)?;
    Ok(())
}

/// Run one setup command in the worktree, through a shell.
///
/// Through a shell because that is what the string in the file looks like —
/// `pnpm install && pnpm build` is one line a developer would type — and the
/// file is the project's own, read from the repository the user pointed the
/// app at.
fn run_command(worktree: &Path, command: &str) -> Result<bool> {
    let mut child = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(worktree)
        .spawn()
        .with_context(|| format!("starting `{command}`"))?;

    let deadline = std::time::Instant::now() + COMMAND_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if std::time::Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            anyhow::bail!(
                "it was still running after {} seconds",
                COMMAND_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_with_no_file_asks_for_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), Setup::default());
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_rather_than_ignored() {
        // It was written on purpose; doing nothing with it silently is how a
        // user ends up debugging a worktree instead of a typo.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ginka")).unwrap();
        std::fs::write(dir.path().join(CONFIG), "{ not json").unwrap();
        assert!(read(dir.path()).is_err());
    }

    #[test]
    fn what_the_repository_does_not_track_is_brought_across() {
        // The whole point: a fresh worktree has no `.env`, and an agent
        // started there fails for a reason that is nothing to do with its
        // task.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(project.join("config")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(project.join(".env"), "TOKEN=secret\n").unwrap();
        std::fs::write(project.join("config/local.json"), "{}").unwrap();

        let setup = Setup {
            copy: vec![".env".into(), "config".into(), "absent.txt".into()],
            commands: Vec::new(),
        };
        let report = run(&project, &worktree, &setup);
        assert_eq!(report.copied, vec![".env", "config"]);
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env")).unwrap(),
            "TOKEN=secret\n"
        );
        assert!(
            worktree.join("config/local.json").is_file(),
            "a directory is copied whole"
        );
        assert!(
            report.problems.is_empty(),
            "a developer without one of these files is not a broken project: {:?}",
            report.problems
        );
    }

    #[test]
    fn a_path_that_climbs_out_of_the_project_is_refused() {
        // This file is in the repository, which means it can arrive in a pull
        // request from anyone.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(dir.path().join("elsewhere.txt"), "not yours\n").unwrap();

        let setup = Setup {
            copy: vec!["../elsewhere.txt".into(), "/etc/hosts".into()],
            commands: Vec::new(),
        };
        let report = run(&project, &worktree, &setup);
        assert!(report.copied.is_empty());
        assert_eq!(report.problems.len(), 2, "{:?}", report.problems);
        assert!(!worktree.join("elsewhere.txt").exists());
    }

    #[test]
    fn commands_run_in_the_new_worktree_and_a_failure_does_not_stop_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        let setup = Setup {
            copy: Vec::new(),
            commands: vec![
                "pwd > where.txt".into(),
                "exit 3".into(),
                "echo done > after.txt".into(),
            ],
        };
        let report = run(&project, &worktree, &setup);
        assert!(
            worktree.join("where.txt").is_file(),
            "the command ran where the work is"
        );
        assert_eq!(
            report.ran.iter().map(|(_, ok)| *ok).collect::<Vec<_>>(),
            vec![true, false, true],
            "one failure is reported, not fatal: the worktree already exists"
        );
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        assert!(worktree.join("after.txt").is_file());
    }

    #[test]
    fn a_setup_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ginka")).unwrap();
        let setup = Setup {
            copy: vec![".env".into()],
            commands: vec!["pnpm install".into()],
        };
        std::fs::write(
            dir.path().join(CONFIG),
            serde_json::to_string_pretty(&setup).unwrap(),
        )
        .unwrap();
        assert_eq!(read(dir.path()).unwrap(), setup);
    }
}
