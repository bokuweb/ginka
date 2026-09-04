//! N7: the review surface asks for a diff by source, and "this turn" is one of
//! the sources.

mod support;

use ginka_core::checkpoint::{Checkpoints, TurnId};
use ginka_core::review::{ChangeStatus, DiffSource, review};
use support::TempRepo;

fn paths(review: &ginka_core::review::Review) -> Vec<&str> {
    review.files.iter().map(|file| file.path.as_str()).collect()
}

#[test]
fn uncommitted_covers_both_staged_and_unstaged_work() {
    let repo = TempRepo::new();
    repo.write("staged.txt", "staged\n");
    repo.git(["add", "staged.txt"]);
    repo.write("README.md", "changed\n");

    let review = review(repo.path(), &DiffSource::Uncommitted).unwrap();
    assert_eq!(paths(&review), ["README.md", "staged.txt"]);
    assert!(review.patch.contains("staged"), "{}", review.patch);
}

#[test]
fn staged_and_unstaged_are_separable() {
    let repo = TempRepo::new();
    repo.write("staged.txt", "staged\n");
    repo.git(["add", "staged.txt"]);
    repo.write("README.md", "changed\n");

    assert_eq!(
        paths(&review(repo.path(), &DiffSource::Staged).unwrap()),
        ["staged.txt"]
    );
    assert_eq!(
        paths(&review(repo.path(), &DiffSource::Unstaged).unwrap()),
        ["README.md"]
    );
}

#[test]
fn an_untracked_file_is_part_of_the_uncommitted_change_set() {
    let repo = TempRepo::new();
    repo.write("new.txt", "one\ntwo\n");

    let review = review(repo.path(), &DiffSource::Uncommitted).unwrap();
    let file = review.files.iter().find(|f| f.path == "new.txt").unwrap();
    assert_eq!(file.status, ChangeStatus::Added);
    assert_eq!(file.insertions, Some(2));
    assert_eq!(file.deletions, Some(0));
    assert!(review.patch.contains("+two"), "{}", review.patch);
}

#[test]
fn an_ignored_file_is_never_part_of_a_change_set() {
    let repo = TempRepo::new();
    repo.write(".gitignore", "secret.env\n");
    repo.commit("ignore secrets");
    repo.write("secret.env", "TOKEN=1\n");

    let review = review(repo.path(), &DiffSource::Uncommitted).unwrap();
    assert!(paths(&review).is_empty(), "{:?}", paths(&review));
}

#[test]
fn a_deleted_file_reads_as_deleted() {
    let repo = TempRepo::new();
    std::fs::remove_file(repo.path().join("README.md")).unwrap();

    let review = review(repo.path(), &DiffSource::Uncommitted).unwrap();
    assert_eq!(review.files[0].status, ChangeStatus::Deleted);
    assert_eq!(review.files[0].deletions, Some(1));
}

#[test]
fn a_binary_file_is_listed_without_line_counts() {
    let repo = TempRepo::new();
    std::fs::write(repo.path().join("blob.bin"), [0u8, 159, 146, 150, 0]).unwrap();
    repo.git(["add", "blob.bin"]);

    let review = review(repo.path(), &DiffSource::Staged).unwrap();
    let file = review.files.iter().find(|f| f.path == "blob.bin").unwrap();
    assert_eq!(file.insertions, None);
    assert!(file.is_binary);
}

#[test]
fn a_clean_worktree_produces_an_empty_review() {
    let repo = TempRepo::new();
    let review = review(repo.path(), &DiffSource::Uncommitted).unwrap();
    assert!(review.files.is_empty());
    assert!(review.patch.is_empty());
}

#[test]
fn a_commit_can_be_reviewed_on_its_own() {
    let repo = TempRepo::new();
    repo.write("added.txt", "hello\n");
    repo.commit("add a file");

    let review = review(repo.path(), &DiffSource::Committed(repo.head())).unwrap();
    assert_eq!(paths(&review), ["added.txt"]);
    assert_eq!(review.files[0].status, ChangeStatus::Added);
}

#[test]
fn a_branch_review_covers_every_commit_since_it_forked_plus_the_working_tree() {
    let repo = TempRepo::new();
    repo.git(["checkout", "-q", "-b", "feature"]);
    repo.write("committed-on-branch.txt", "a\n");
    repo.commit("branch work");
    repo.write("still-uncommitted.txt", "b\n");

    let review = review(
        repo.path(),
        &DiffSource::Branch {
            base: "main".into(),
        },
    )
    .unwrap();
    assert_eq!(
        paths(&review),
        ["committed-on-branch.txt", "still-uncommitted.txt"]
    );
}

#[test]
fn a_turn_review_shows_only_what_that_turn_changed() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    let turn = TurnId::new("session-a", 1);

    repo.write("by-hand.txt", "mine\n");
    checkpoints.capture_turn_start(&turn).unwrap();
    repo.write("by-agent.txt", "theirs\n");
    checkpoints.capture_turn_end(&turn).unwrap();

    let review = review(repo.path(), &DiffSource::Turn(turn)).unwrap();
    assert_eq!(paths(&review), ["by-agent.txt"]);
    assert_eq!(review.files[0].insertions, Some(1));
}

#[test]
fn a_turn_that_was_never_checkpointed_reviews_as_empty() {
    let repo = TempRepo::new();
    let review = review(repo.path(), &DiffSource::Turn(TurnId::new("nope", 3))).unwrap();
    assert!(review.files.is_empty());
    assert!(review.patch.is_empty());
}

#[test]
fn totals_are_summed_across_the_change_set() {
    let repo = TempRepo::new();
    repo.write("a.txt", "1\n2\n3\n");
    repo.write("b.txt", "1\n");
    repo.git(["add", "."]);

    let review = review(repo.path(), &DiffSource::Staged).unwrap();
    assert_eq!(review.insertions(), 4);
    assert_eq!(review.deletions(), 0);
}
