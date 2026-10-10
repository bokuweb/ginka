//! Selected-file commits preserve the index belonging to other work.

mod support;

use ginka_core::git;
use support::TempRepo;

fn paths(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn selected_commit_preserves_unrelated_partial_staging() {
    let repo = TempRepo::new();
    repo.write("other.txt", "original\n");
    repo.commit("Add other");
    repo.write("other.txt", "staged\n");
    repo.git(["add", "other.txt"]);
    repo.write("other.txt", "staged and later\n");
    repo.write("README.md", "selected\n");
    let staged = repo.git(["ls-files", "--stage", "other.txt"]);
    git::commit_selected(repo.path(), "Select readme", &paths(&["README.md"])).unwrap();
    assert_eq!(repo.git(["show", "HEAD:README.md"]), "selected");
    assert_eq!(repo.git(["show", "HEAD:other.txt"]), "original");
    assert_eq!(repo.git(["ls-files", "--stage", "other.txt"]), staged);
    assert_eq!(repo.read("other.txt").unwrap(), "staged and later\n");
    assert_eq!(repo.git(["diff", "--cached", "--name-only"]), "other.txt");
}

#[test]
fn selected_files_use_complete_worktree_contents_and_literal_names() {
    let repo = TempRepo::new();
    repo.write("README.md", "staged\n");
    repo.git(["add", "README.md"]);
    repo.write("README.md", "latest\n");
    repo.write("[new].txt", "new\n");
    repo.write("n.txt", "unselected\n");
    git::commit_selected(
        repo.path(),
        "Select files",
        &paths(&["README.md", "[new].txt"]),
    )
    .unwrap();
    assert_eq!(repo.git(["show", "HEAD:README.md"]), "latest");
    assert_eq!(repo.git(["show", "HEAD:[new].txt"]), "new");
    assert!(
        !repo
            .git(["ls-tree", "--name-only", "HEAD"])
            .contains("n.txt")
    );
    assert!(repo.git(["diff", "--cached", "--name-only"]).is_empty());
}

#[test]
fn selecting_a_rename_includes_its_old_path_and_a_staged_deletion() {
    let repo = TempRepo::new();
    repo.write("remove.txt", "remove\n");
    repo.commit("Add removal");
    repo.git(["mv", "README.md", "renamed.md"]);
    repo.git(["rm", "remove.txt"]);
    git::commit_selected(
        repo.path(),
        "Rename and delete",
        &paths(&["renamed.md", "remove.txt"]),
    )
    .unwrap();
    assert_eq!(repo.git(["ls-tree", "--name-only", "HEAD"]), "renamed.md");
    assert!(repo.git(["status", "--porcelain"]).is_empty());
}

#[test]
fn a_refused_hook_leaves_the_real_index_byte_for_byte_unchanged() {
    let repo = TempRepo::new();
    repo.write("README.md", "selected\n");
    repo.write("other.txt", "staged\n");
    repo.git(["add", "other.txt"]);
    let hooks = repo.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'refused selection' >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    repo.git(["config", "core.hooksPath", hooks.to_str().unwrap()]);
    let index = repo.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let head = repo.head();
    let error = git::commit_selected(repo.path(), "Selected", &paths(&["README.md"])).unwrap_err();
    assert!(error.to_string().contains("refused selection"), "{error:#}");
    assert_eq!(repo.head(), head);
    assert_eq!(std::fs::read(&index).unwrap(), before);
    assert!(!repo.path().join(".git/index.lock").exists());
}

#[test]
fn selections_reject_directories_traversal_empty_and_foreign_index_locks() {
    let repo = TempRepo::new();
    repo.write("dir/a.txt", "a\n");
    let before = std::fs::read(repo.path().join(".git/index")).unwrap();
    for names in [
        &[][..],
        &["."][..],
        &["dir"][..],
        &["../outside"][..],
        &["missing"][..],
    ] {
        assert!(git::commit_selected(repo.path(), "Selected", &paths(names)).is_err());
        assert_eq!(
            std::fs::read(repo.path().join(".git/index")).unwrap(),
            before
        );
    }
    let lock = repo.path().join(".git/index.lock");
    std::fs::write(&lock, "owned elsewhere").unwrap();
    assert!(git::commit_selected(repo.path(), "Selected", &paths(&["README.md"])).is_err());
    assert_eq!(std::fs::read_to_string(lock).unwrap(), "owned elsewhere");
}

#[test]
fn a_deleted_directory_cannot_expand_a_file_selection() {
    let repo = TempRepo::new();
    repo.write("removed/a.txt", "a\n");
    repo.write("removed/b.txt", "b\n");
    repo.commit("Add directory");
    repo.git(["rm", "-r", "removed"]);
    let index = repo.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let head = repo.head();
    assert!(git::describe_selected(repo.path(), &paths(&["removed"])).is_err());
    assert!(git::commit_selected(repo.path(), "Selected", &paths(&["removed"])).is_err());
    assert_eq!(repo.head(), head);
    assert_eq!(std::fs::read(index).unwrap(), before);
}

#[test]
fn a_rename_source_replaced_by_a_directory_cannot_add_unselected_files() {
    let repo = TempRepo::new();
    repo.git(["mv", "README.md", "renamed.md"]);
    repo.write("README.md/unselected.txt", "unselected\n");
    let index = repo.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let head = repo.head();
    assert!(git::describe_selected(repo.path(), &paths(&["renamed.md"])).is_err());
    assert!(git::commit_selected(repo.path(), "Selected", &paths(&["renamed.md"])).is_err());
    assert_eq!(repo.head(), head);
    assert_eq!(std::fs::read(index).unwrap(), before);
}

#[test]
fn descriptions_include_only_selected_changes_without_touching_the_index() {
    let repo = TempRepo::new();
    repo.write("new.txt", "selected contents\n");
    repo.write("README.md", "unrelated contents\n");
    repo.git(["add", "README.md"]);
    let before = std::fs::read(repo.path().join(".git/index")).unwrap();
    let (files, diff) = git::describe_selected(repo.path(), &paths(&["new.txt"])).unwrap();
    assert_eq!(files, paths(&["new.txt"]));
    assert!(diff.contains("+selected contents"));
    assert!(!diff.contains("unrelated contents"));
    assert_eq!(
        std::fs::read(repo.path().join(".git/index")).unwrap(),
        before
    );
}

#[test]
fn selected_commits_work_in_a_linked_worktree() {
    let repo = TempRepo::new();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked");
    repo.git(["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
    std::fs::write(linked.join("README.md"), "linked\n").unwrap();
    let original = std::fs::read(repo.path().join(".git/index")).unwrap();
    git::commit_selected(&linked, "Selected linked", &paths(&["README.md"])).unwrap();
    assert_eq!(repo.git_in(&linked, ["show", "HEAD:README.md"]), "linked");
    assert_eq!(
        std::fs::read(repo.path().join(".git/index")).unwrap(),
        original
    );
}

#[test]
fn a_root_commit_keeps_unselected_staged_files() {
    let repo = TempRepo::new();
    repo.git(["checkout", "--orphan", "root"]);
    repo.write("new.txt", "selected\n");
    git::commit_selected(repo.path(), "Root selection", &paths(&["new.txt"])).unwrap();
    assert_eq!(repo.git(["ls-tree", "--name-only", "HEAD"]), "new.txt");
    assert_eq!(repo.git(["diff", "--cached", "--name-only"]), "README.md");
}

#[test]
fn a_split_index_keeps_unrelated_staged_entries() {
    let repo = TempRepo::new();
    repo.write("other.txt", "original\n");
    repo.commit("Add other");
    repo.write("other.txt", "staged\n");
    repo.git(["add", "other.txt"]);
    repo.git(["update-index", "--split-index"]);
    repo.write("README.md", "selected\n");
    let staged = repo.git(["ls-files", "--stage", "other.txt"]);
    git::commit_selected(repo.path(), "Select readme", &paths(&["README.md"])).unwrap();
    assert_eq!(repo.git(["ls-files", "--stage", "other.txt"]), staged);
    assert_eq!(repo.git(["show", "HEAD:README.md"]), "selected");
    assert_eq!(repo.git(["show", "HEAD:other.txt"]), "original");
}

#[test]
fn a_merge_in_progress_cannot_be_partially_committed() {
    let repo = TempRepo::new();
    repo.git(["checkout", "-b", "side"]);
    repo.write("side.txt", "side\n");
    repo.commit("Side change");
    repo.git(["checkout", "-"]);
    repo.write("main.txt", "main\n");
    repo.commit("Main change");
    repo.git(["merge", "--no-commit", "side"]);
    let index = repo.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let head = repo.head();
    assert!(git::commit_selected(repo.path(), "Partial merge", &paths(&["side.txt"])).is_err());
    assert_eq!(repo.head(), head);
    assert_eq!(std::fs::read(index).unwrap(), before);
    assert!(repo.path().join(".git/MERGE_HEAD").exists());
}

#[test]
fn whitespace_in_selected_paths_is_preserved() {
    let repo = TempRepo::new();
    let name = " leading\ntrailing ";
    repo.write(name, "selected\n");
    git::commit_selected(repo.path(), "Select literal whitespace", &paths(&[name])).unwrap();
    assert_eq!(repo.git(["show", &format!("HEAD:{name}")]), "selected");
    assert!(repo.git(["status", "--porcelain"]).is_empty());
}
