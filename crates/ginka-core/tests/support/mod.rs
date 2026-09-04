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
