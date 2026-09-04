//! A real git repository in a temporary directory.
//!
//! Worktree and checkpoint behaviour is pinned against real git rather than a
//! mock: every edge case that has ever mattered here (untracked files, a
//! branch switched underneath us, a detached head) is a property of git, and a
//! mock would only ever confirm our own assumptions.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TempRepo {
    dir: tempfile::TempDir,
}

impl TempRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Self { dir };
        repo.git(["init", "-q", "-b", "main"]);
        repo.git(["config", "user.email", "test@ginka.invalid"]);
        repo.git(["config", "user.name", "Ginka Test"]);
        repo.git(["config", "commit.gpgsign", "false"]);
        repo.write("README.md", "start\n");
        repo.git(["add", "."]);
        repo.commit("first");
        repo
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    pub fn read(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(relative)).ok()
    }

    pub fn commit(&self, message: &str) {
        self.git(["add", "-A"]);
        self.git(["commit", "-q", "-m", message]);
    }

    pub fn head(&self) -> String {
        self.git(["rev-parse", "HEAD"])
    }

    pub fn git<const N: usize>(&self, args: [&str; N]) -> String {
        self.git_in(self.dir.path(), args)
    }

    pub fn git_in<const N: usize>(&self, cwd: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git is required to run these tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn refs(&self) -> Vec<String> {
        self.git(["for-each-ref", "--format=%(refname)"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn into_path(self) -> PathBuf {
        self.dir.keep()
    }
}

// ---------------------------------------------------------------------------
// Helpers the daemon and CLI tests use, from the same repository fixtures.

/// Run `git` in `repo`, asserting it succeeded.
pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git is on PATH");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository with one commit on `main`, and no remote.
///
/// Idempotent: a test that registers the same project twice — which is how a
/// user tells Ginka a repository moved — must not re-run `git init` and then
/// fail on an empty commit.
pub fn repository(root: &Path) -> PathBuf {
    if root.join(".git").exists() {
        return root.to_path_buf();
    }
    std::fs::create_dir_all(root).unwrap();
    git(root, &["init", "--initial-branch=main"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    // Commit signing and hooks belong to the developer, not to the test.
    git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "first"]);
    root.to_path_buf()
}
