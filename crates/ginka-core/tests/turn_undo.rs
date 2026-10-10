//! Guarded undo preserves the state handed to an agent and refuses newer work.

mod support;

use ginka_core::checkpoint::undo::UndoState;
use ginka_core::checkpoint::{Checkpoints, TurnId};
use support::TempRepo;

fn finish(
    repo: &TempRepo,
    checkpoints: &Checkpoints,
    start: &ginka_core::checkpoint::TurnStart,
) -> UndoState {
    let end = ginka_core::git::snapshot(repo.path(), "refs/ginka/test-end", "end").unwrap();
    checkpoints.capture_undo(start, &end).unwrap().unwrap()
}

#[test]
fn undo_preserves_partial_staging_untracked_work_and_head() {
    let repo = TempRepo::new();
    repo.write("README.md", "staged\n");
    repo.git(["add", "README.md"]);
    repo.write("README.md", "hand edited\n");
    repo.write("notes.txt", "my notes\n");
    let head = repo.head();
    let checkpoints = Checkpoints::new(repo.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 1))
        .unwrap();
    repo.write("README.md", "agent\n");
    repo.write("added.txt", "agent\n");
    repo.git(["add", "-A"]);
    let undo = finish(&repo, &checkpoints, &start);
    checkpoints.undo_turn(&undo).unwrap();
    assert_eq!(repo.read("README.md").as_deref(), Some("hand edited\n"));
    assert_eq!(repo.git(["show", ":README.md"]), "staged");
    assert_eq!(repo.read("notes.txt").as_deref(), Some("my notes\n"));
    assert_eq!(repo.read("added.txt"), None);
    assert_eq!(repo.git(["ls-files", "notes.txt"]), "");
    assert_eq!(repo.head(), head);
}

#[test]
fn later_file_and_index_edits_are_refused_without_changes() {
    for stage_only in [false, true] {
        let repo = TempRepo::new();
        let checkpoints = Checkpoints::new(repo.path());
        let start = checkpoints
            .capture_turn_start(&TurnId::new("s", 1))
            .unwrap();
        repo.write("README.md", "agent\n");
        let undo = finish(&repo, &checkpoints, &start);
        if stage_only {
            repo.git(["add", "README.md"]);
        } else {
            repo.write("new.txt", "newer work\n");
        }
        let status = repo.git(["status", "--porcelain"]);
        assert!(
            checkpoints
                .undo_turn(&undo)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        assert_eq!(repo.git(["status", "--porcelain"]), status);
        assert_eq!(repo.read("README.md").as_deref(), Some("agent\n"));
    }
}

#[test]
fn changed_head_or_branch_is_refused() {
    for commit in [false, true] {
        let repo = TempRepo::new();
        let checkpoints = Checkpoints::new(repo.path());
        let start = checkpoints
            .capture_turn_start(&TurnId::new("s", 1))
            .unwrap();
        repo.write("README.md", "agent\n");
        let undo = finish(&repo, &checkpoints, &start);
        if commit {
            repo.commit("later commit");
        } else {
            repo.git(["switch", "-q", "-c", "other"]);
        }
        assert!(
            checkpoints
                .undo_turn(&undo)
                .unwrap_err()
                .to_string()
                .contains("HEAD")
        );
        assert_eq!(repo.read("README.md").as_deref(), Some("agent\n"));
    }
}

#[test]
fn a_commit_during_the_turn_cannot_be_undone_without_rewriting_history() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 1))
        .unwrap();
    repo.write("README.md", "agent\n");
    repo.commit("agent commit");
    let undo = finish(&repo, &checkpoints, &start);
    assert!(checkpoints.undo_turn(&undo).is_err());
    assert_eq!(repo.read("README.md").as_deref(), Some("agent\n"));
}

#[test]
fn captures_work_in_linked_and_unborn_worktrees() {
    let repo = TempRepo::new();
    let linked = tempfile::tempdir().unwrap();
    repo.git([
        "worktree",
        "add",
        "-q",
        "-b",
        "linked",
        linked.path().to_str().unwrap(),
    ]);
    Checkpoints::new(linked.path())
        .capture_turn_start(&TurnId::new("linked", 1))
        .unwrap();
    let unborn = tempfile::tempdir().unwrap();
    repo.git_in(unborn.path(), ["init", "-q", "-b", "empty"]);
    let checkpoints = Checkpoints::new(unborn.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("empty", 1))
        .unwrap();
    std::fs::write(unborn.path().join("new.txt"), "agent").unwrap();
    let end = ginka_core::git::snapshot(unborn.path(), "refs/ginka/test-end", "end").unwrap();
    let undo = checkpoints.capture_undo(&start, &end).unwrap().unwrap();
    checkpoints.undo_turn(&undo).unwrap();
    assert!(!unborn.path().join("new.txt").exists());
    assert_eq!(repo.git_in(unborn.path(), ["status", "--porcelain"]), "");
}

#[test]
fn overlapping_paths_use_components_and_resolve_symlinks() {
    use ginka_core::checkpoint::undo::paths_overlap;
    let dir = tempfile::tempdir().unwrap();
    assert!(paths_overlap(dir.path(), &dir.path().join("nested")));
    assert!(!paths_overlap(
        &dir.path().join("one"),
        &dir.path().join("one-other")
    ));
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        let target = dir.path().join("real");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert!(paths_overlap(&alias, &target));
    }
}

#[test]
fn ignored_files_survive_but_a_restoration_collision_is_refused() {
    let repo = TempRepo::new();
    repo.write(".gitignore", "*.secret\n");
    repo.commit("ignore secrets");
    repo.write("kept.secret", "before\n");
    repo.git(["add", "-f", "kept.secret"]);
    let checkpoints = Checkpoints::new(repo.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 1))
        .unwrap();
    repo.git(["rm", "-q", "-f", "kept.secret"]);
    repo.write("README.md", "agent\n");
    let undo = finish(&repo, &checkpoints, &start);
    repo.write("kept.secret", "later secret\n");
    repo.write("unrelated.secret", "leave me\n");
    assert!(
        checkpoints
            .undo_turn(&undo)
            .unwrap_err()
            .to_string()
            .contains("overwritten")
    );
    assert_eq!(repo.read("kept.secret").as_deref(), Some("later secret\n"));
    assert_eq!(repo.read("README.md").as_deref(), Some("agent\n"));
    std::fs::remove_file(repo.path().join("kept.secret")).unwrap();
    checkpoints.undo_turn(&undo).unwrap();
    assert_eq!(repo.read("kept.secret").as_deref(), Some("before\n"));
    assert_eq!(repo.read("unrelated.secret").as_deref(), Some("leave me\n"));
    assert_eq!(repo.git(["show", ":kept.secret"]), "before");
}

#[test]
fn exposing_preexisting_ignored_files_keeps_the_checkpoint_without_undo() {
    for (path, force_add) in ["kept.secret", "noise/existing.txt", " spaced\n秘密.secret"]
        .into_iter()
        .flat_map(|path| [false, true].map(|force_add| (path, force_add)))
    {
        let repo = TempRepo::new();
        repo.write(".gitignore", "*.secret\nnoise/\n");
        repo.commit("ignore local files");
        repo.write(path, "preexisting local file\n");
        let checkpoints = Checkpoints::new(repo.path());
        let start = checkpoints
            .capture_turn_start(&TurnId::new("s", 1))
            .unwrap();
        if force_add {
            repo.git(["add", "-f", path]);
        } else {
            repo.write(".gitignore", "");
        }
        let end = ginka_core::git::snapshot(repo.path(), "refs/ginka/test-end", "end").unwrap();
        assert!(checkpoints.capture_undo(&start, &end).unwrap().is_none());
        assert_eq!(repo.read(path).as_deref(), Some("preexisting local file\n"));
    }
}

// macOS rejects these names at creation; Linux permits them in Git worktrees.
#[cfg(target_os = "linux")]
#[test]
fn unreadable_ignored_paths_disable_undo_without_losing_the_checkpoint() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let repo = TempRepo::new();
    repo.write(".gitignore", "*.secret\n");
    repo.commit("ignore local files");
    let path = repo
        .path()
        .join(OsString::from_vec(b"bad\xff.secret".to_vec()));
    std::fs::write(&path, "keep me\n").unwrap();
    let checkpoints = Checkpoints::new(repo.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 1))
        .unwrap();
    assert!(start.ignored_paths.is_none());
    repo.write("README.md", "agent\n");
    let end = ginka_core::git::snapshot(repo.path(), "refs/ginka/test-end", "end").unwrap();
    assert!(checkpoints.capture_undo(&start, &end).unwrap().is_none());
    assert_eq!(repo.git(["show", &format!("{end}:README.md")]), "agent");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "keep me\n");
}

#[test]
fn unsupported_index_flags_keep_ordinary_checkpoints_without_undo() {
    let repo = TempRepo::new();
    repo.git(["update-index", "--assume-unchanged", "README.md"]);
    repo.write("README.md", "hand edit\n");
    let checkpoints = Checkpoints::new(repo.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 1))
        .unwrap();
    assert!(start.index_commit.is_none());
    assert_eq!(
        repo.git(["show", &format!("{}:README.md", start.commit)]),
        "hand edit"
    );
    assert!(repo.git(["ls-files", "-v"]).starts_with("h "));
    repo.git(["update-index", "--no-assume-unchanged", "README.md"]);
    repo.write("pending.txt", "pending\n");
    repo.git(["add", "-N", "pending.txt"]);
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 2))
        .unwrap();
    assert!(start.index_commit.is_none());
    assert_eq!(
        repo.git(["show", &format!("{}:pending.txt", start.commit)]),
        "pending"
    );
}

#[test]
fn split_index_and_index_lock_are_handled_without_losing_staging() {
    let repo = TempRepo::new();
    repo.git(["update-index", "--split-index"]);
    repo.write("README.md", "staged\n");
    repo.git(["add", "README.md"]);
    repo.write("README.md", "hand edit\n");
    let checkpoints = Checkpoints::new(repo.path());
    let start = checkpoints
        .capture_turn_start(&TurnId::new("s", 1))
        .unwrap();
    repo.write("README.md", "agent\n");
    let undo = finish(&repo, &checkpoints, &start);
    let lock = repo.path().join(".git/index.lock");
    std::fs::write(&lock, "another operation").unwrap();
    assert!(
        checkpoints
            .undo_turn(&undo)
            .unwrap_err()
            .to_string()
            .contains("locked")
    );
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "another operation");
    std::fs::remove_file(&lock).unwrap();
    checkpoints.undo_turn(&undo).unwrap();
    assert_eq!(repo.read("README.md").as_deref(), Some("hand edit\n"));
    assert_eq!(repo.git(["show", ":README.md"]), "staged");
    assert!(!lock.exists());
}
