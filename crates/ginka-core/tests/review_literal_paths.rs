//! Review actions address literal paths, even when Git would read them as patterns.

mod support;

use ginka_core::git;
use support::TempRepo;

fn staged_paths(repo: &TempRepo) -> Vec<String> {
    repo.git(["diff", "--cached", "--name-only", "-z"])
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

fn edited_pair(target: &str, neighbour: &str) -> TempRepo {
    let repo = TempRepo::new();
    repo.write(target, "original\n");
    repo.write(neighbour, "original\n");
    repo.commit("add review fixtures");
    repo.write(target, "target edit\n");
    repo.write(neighbour, "neighbour edit\n");
    repo
}

#[test]
fn staging_and_unstaging_a_literal_bracket_name_preserves_its_neighbour() {
    let repo = edited_pair("[ab].txt", "a.txt");
    git::stage(repo.path(), "[ab].txt").unwrap();
    assert_eq!(staged_paths(&repo), ["[ab].txt"]);

    git::stage(repo.path(), "a.txt").unwrap();
    git::unstage(repo.path(), "[ab].txt").unwrap();
    assert_eq!(staged_paths(&repo), ["a.txt"]);
    assert_eq!(repo.read("[ab].txt").as_deref(), Some("target edit\n"));
}

#[cfg(unix)]
#[test]
fn staging_and_unstaging_literal_wildcards_and_magic_preserves_other_paths() {
    for target in ["*.txt", "?.txt", ":(top)a.txt", ":(exclude)a.txt"] {
        let repo = edited_pair(target, "a.txt");
        git::stage(repo.path(), target).unwrap();
        assert_eq!(staged_paths(&repo), [target], "stage {target}");
        git::stage(repo.path(), "a.txt").unwrap();
        git::unstage(repo.path(), target).unwrap();
        assert_eq!(staged_paths(&repo), ["a.txt"], "unstage {target}");
    }
}

#[test]
fn staging_a_literal_directory_includes_only_its_descendants() {
    let repo = TempRepo::new();
    repo.write("[ab]/one.txt", "one\n");
    repo.write("[ab]/nested/two.txt", "two\n");
    repo.write("a/other.txt", "other\n");
    git::stage(repo.path(), "[ab]").unwrap();
    assert_eq!(staged_paths(&repo), ["[ab]/nested/two.txt", "[ab]/one.txt"]);
    git::stage(repo.path(), "a").unwrap();
    git::unstage(repo.path(), "[ab]").unwrap();
    assert_eq!(staged_paths(&repo), ["a/other.txt"]);
}

#[test]
fn a_missing_literal_path_cannot_match_an_existing_file() {
    let repo = TempRepo::new();
    repo.write("a.txt", "keep\n");
    assert!(git::stage(repo.path(), "[ab].txt").is_err());
    assert!(staged_paths(&repo).is_empty());
    git::stage(repo.path(), "a.txt").unwrap();
    assert!(git::unstage(repo.path(), "[ab].txt").is_err());
    assert_eq!(staged_paths(&repo), ["a.txt"]);
}

#[test]
fn unstaging_before_the_first_commit_removes_only_the_literal_index_entry() {
    let dir = tempfile::tempdir().unwrap();
    support::git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("[ab].txt"), "target\n").unwrap();
    std::fs::write(dir.path().join("a.txt"), "neighbour\n").unwrap();
    support::git(dir.path(), &["add", "-A"]);
    git::unstage(dir.path(), "[ab].txt").unwrap();
    assert_eq!(support::git(dir.path(), &["ls-files"]), "a.txt");
    assert!(dir.path().join("[ab].txt").is_file());
}

#[test]
fn unstaging_a_literal_directory_before_the_first_commit_keeps_other_entries() {
    let dir = tempfile::tempdir().unwrap();
    support::git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("[ab]/nested")).unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::write(dir.path().join("[ab]/one.txt"), "one\n").unwrap();
    std::fs::write(dir.path().join("[ab]/nested/two.txt"), "two\n").unwrap();
    std::fs::write(dir.path().join("a/other.txt"), "other\n").unwrap();
    support::git(dir.path(), &["add", "-A"]);
    git::unstage(dir.path(), "[ab]").unwrap();
    assert_eq!(support::git(dir.path(), &["ls-files"]), "a/other.txt");
    assert!(dir.path().join("[ab]/one.txt").is_file());
    assert!(dir.path().join("[ab]/nested/two.txt").is_file());
}

#[test]
fn a_deleted_literal_file_can_be_staged_and_unstaged() {
    let repo = edited_pair("[ab].txt", "a.txt");
    std::fs::remove_file(repo.path().join("[ab].txt")).unwrap();
    git::stage(repo.path(), "[ab].txt").unwrap();
    assert_eq!(staged_paths(&repo), ["[ab].txt"]);
    git::unstage(repo.path(), "[ab].txt").unwrap();
    assert!(staged_paths(&repo).is_empty());
    assert!(!repo.path().join("[ab].txt").exists());
}

#[test]
fn reverting_a_tracked_literal_file_leaves_other_index_and_worktree_edits() {
    let repo = edited_pair("[ab].txt", "a.txt");
    repo.git(["add", "-A"]);
    repo.write("a.txt", "newer neighbour edit\n");
    git::revert_file(repo.path(), "[ab].txt").unwrap();
    assert_eq!(repo.read("[ab].txt").as_deref(), Some("original\n"));
    assert_eq!(
        repo.read("a.txt").as_deref(),
        Some("newer neighbour edit\n")
    );
    assert_eq!(staged_paths(&repo), ["a.txt"]);
}

#[test]
fn reverting_an_added_literal_file_keeps_its_neighbours_index_entry() {
    let repo = TempRepo::new();
    repo.write("[ab].txt", "target\n");
    repo.write("a.txt", "neighbour\n");
    repo.git(["add", "-A"]);
    git::revert_file(repo.path(), "[ab].txt").unwrap();
    assert!(!repo.path().join("[ab].txt").exists());
    assert_eq!(staged_paths(&repo), ["a.txt"]);
    assert_eq!(repo.read("a.txt").as_deref(), Some("neighbour\n"));
}

#[test]
fn hunk_stage_unstage_and_revert_select_the_literal_file() {
    for target in [
        "[ab].txt",
        #[cfg(unix)]
        ":(top)a.txt",
    ] {
        let repo = edited_pair(target, "a.txt");
        let header = "@@ -1 +1 @@";
        git::stage_hunk(repo.path(), target, header, true).unwrap();
        assert_eq!(staged_paths(&repo), [target]);
        git::stage(repo.path(), "a.txt").unwrap();
        git::stage_hunk(repo.path(), target, header, false).unwrap();
        assert_eq!(staged_paths(&repo), ["a.txt"]);
        git::revert_hunk(repo.path(), target, header).unwrap();
        assert_eq!(repo.read(target).as_deref(), Some("original\n"));
        assert_eq!(repo.read("a.txt").as_deref(), Some("neighbour edit\n"));
        assert_eq!(staged_paths(&repo), ["a.txt"]);
    }
}

#[cfg(unix)]
#[test]
fn a_new_literal_hunk_does_not_add_neighbours_to_the_index() {
    let repo = TempRepo::new();
    repo.write("*.txt", "target\n");
    repo.write("other.txt", "neighbour\n");
    git::stage_hunk(repo.path(), "*.txt", "@@ -0,0 +1 @@", true).unwrap();
    assert_eq!(staged_paths(&repo), ["*.txt"]);
    assert_eq!(repo.git(["ls-files", "--", "other.txt"]), "");
    git::stage_hunk(repo.path(), "*.txt", "@@ -0,0 +1 @@", false).unwrap();
    git::revert_hunk(repo.path(), "*.txt", "@@ -0,0 +1 @@").unwrap();
    assert!(!repo.path().join("*.txt").exists());
    assert_eq!(repo.read("other.txt").as_deref(), Some("neighbour\n"));
    assert_eq!(repo.git(["ls-files", "--", "other.txt"]), "");
}
