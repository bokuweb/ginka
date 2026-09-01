//! Throwaway git repositories for the integration tests.
//!
//! Every test that touches git builds its own repository in a `tempfile` dir
//! and points `GINKA_HOME` at another one, so nothing here can reach the
//! developer's real state.

use std::path::{Path, PathBuf};
use std::process::Command;

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
