//! Fan-out's last step (Orca): the attempt that won is merged into the branch
//! the project is on, and a merge that cannot be clean leaves nothing behind.

mod support;

use ginka_core::git;
use std::path::{Path, PathBuf};
use support::{git as run, repository};

/// A repository on `main` and an attempt worktree on `try-1` cut from it.
fn race(root: &Path) -> (PathBuf, PathBuf) {
    let repo = repository(&root.join("comet"));
    let attempt = root.join("attempt");
    git::add_worktree(&repo, &attempt, "try-1", "main").unwrap();
    (repo, attempt)
}

fn commit_file(worktree: &Path, name: &str, text: &str) {
    std::fs::write(worktree.join(name), text).unwrap();
    run(worktree, &["add", "."]);
    run(worktree, &["commit", "-m", name]);
}

#[test]
fn an_attempt_ahead_of_the_base_is_fast_forwarded_into_it() {
    let work = tempfile::tempdir().unwrap();
    let (repo, attempt) = race(work.path());
    commit_file(&attempt, "answer.txt", "42\n");

    let merged = git::merge_into(&repo, "try-1", "main").unwrap();
    assert!(merged.fast_forward);
    assert_eq!(merged.into, "main");
    assert_eq!(Some(merged.commit.clone()), git::head_commit(&attempt));
    assert_eq!(git::head_commit(&repo), Some(merged.commit));
    assert_eq!(
        std::fs::read_to_string(repo.join("answer.txt")).unwrap(),
        "42\n",
        "the checkout of the base shows the result, not just its ref"
    );
}

#[test]
fn a_base_that_moved_on_gets_a_merge_commit_with_both_sides() {
    let work = tempfile::tempdir().unwrap();
    let (repo, attempt) = race(work.path());
    commit_file(&attempt, "answer.txt", "42\n");
    commit_file(&repo, "other.txt", "meanwhile\n");

    let merged = git::merge_into(&repo, "try-1", "main").unwrap();
    assert!(!merged.fast_forward);
    assert_eq!(git::head_commit(&repo), Some(merged.commit));
    assert!(repo.join("answer.txt").exists());
    assert!(repo.join("other.txt").exists());
}

#[test]
fn a_conflict_is_named_and_undone() {
    let work = tempfile::tempdir().unwrap();
    let (repo, attempt) = race(work.path());
    commit_file(&attempt, "README.md", "attempt\n");
    commit_file(&repo, "README.md", "base\n");
    let before = git::head_commit(&repo);

    let error = git::merge_into(&repo, "try-1", "main").unwrap_err();
    assert!(format!("{error:#}").contains("README.md"), "{error:#}");
    assert_eq!(git::head_commit(&repo), before);
    let status = git::branch_status(&repo).unwrap();
    assert!(!status.dirty && !status.conflict, "the merge was aborted");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "base\n"
    );
}

#[test]
fn a_base_checkout_with_uncommitted_work_is_not_merged_into() {
    let work = tempfile::tempdir().unwrap();
    let (repo, attempt) = race(work.path());
    commit_file(&attempt, "answer.txt", "42\n");
    std::fs::write(repo.join("README.md"), "half-written\n").unwrap();
    let before = git::head_commit(&repo);

    let error = git::merge_into(&repo, "try-1", "main").unwrap_err();
    assert!(format!("{error:#}").contains("uncommitted"), "{error:#}");
    assert_eq!(git::head_commit(&repo), before);
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "half-written\n",
        "the reader's work is untouched"
    );
}

#[test]
fn a_base_checked_out_nowhere_moves_only_by_fast_forward() {
    let work = tempfile::tempdir().unwrap();
    let (repo, attempt) = race(work.path());
    run(&repo, &["branch", "release"]);
    commit_file(&attempt, "answer.txt", "42\n");

    let merged = git::merge_into(&repo, "try-1", "release").unwrap();
    assert!(merged.fast_forward);
    assert_eq!(
        run(&repo, &["rev-parse", "release"]).trim(),
        merged.commit,
        "the ref moved"
    );
    assert_eq!(
        run(&repo, &["rev-parse", "HEAD"]).trim(),
        git::head_commit(&repo).unwrap(),
    );

    // Diverged, with no checkout to resolve a merge in: refused.
    run(&repo, &["branch", "-f", "stale", "main"]);
    let stale_worktree = work.path().join("stale");
    git::add_worktree(&repo, &stale_worktree, "stale", "main").unwrap();
    commit_file(&stale_worktree, "elsewhere.txt", "x\n");
    git::remove_worktree(&repo, &stale_worktree, false).unwrap();
    let error = git::merge_into(&repo, "try-1", "stale").unwrap_err();
    assert!(format!("{error:#}").contains("check out"), "{error:#}");
}

#[test]
fn a_branch_is_never_taken_for_an_option() {
    let work = tempfile::tempdir().unwrap();
    let (repo, _) = race(work.path());
    assert!(git::merge_into(&repo, "--help", "main").is_err());
    assert!(git::merge_into(&repo, "try-1", "-f").is_err());
}
