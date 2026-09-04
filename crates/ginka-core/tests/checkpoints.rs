//! N8: a checkpoint is three refs per turn, so a hand edit made between turns
//! is never attributed to the agent.

mod support;

use ginka_core::checkpoint::{Checkpoints, TurnId};
use support::TempRepo;

fn turn(n: usize) -> TurnId {
    TurnId::new("11111111-2222-3333-4444-555555555555", n)
}

#[test]
fn starting_a_turn_records_the_state_it_was_handed() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());

    let start = checkpoints.capture_turn_start(&turn(1)).unwrap();
    assert_eq!(start.base_commit.as_deref(), Some(repo.head().as_str()));
    assert!(checkpoints.exists(&turn(1)).unwrap());
}

#[test]
fn its_refs_live_in_our_own_namespace_and_never_look_like_branches() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    checkpoints.capture_turn_start(&turn(1)).unwrap();
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    let refs = repo.refs();
    assert!(
        refs.iter().any(|name| name.starts_with("refs/ginka/")),
        "{refs:?}"
    );
    assert!(
        !refs
            .iter()
            .any(|name| name.starts_with("refs/heads/") && name.contains("ginka")),
        "checkpoints must not appear in the branch list: {refs:?}"
    );
}

#[test]
fn a_hand_edit_made_between_turns_is_not_attributed_to_the_agent() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());

    // The user edits a file in the terminal after the previous turn ended.
    repo.write("notes.md", "written by hand\n");

    // The turn starts: this is the state it was handed.
    checkpoints.capture_turn_start(&turn(1)).unwrap();

    // The agent edits something else.
    repo.write("src.rs", "fn main() {}\n");
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    let changed = checkpoints.changed_files(&turn(1)).unwrap();
    assert_eq!(changed, vec!["src.rs".to_string()]);
}

#[test]
fn a_file_the_agent_deletes_is_part_of_its_turn() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    repo.write("doomed.txt", "bye\n");
    repo.commit("add doomed");

    checkpoints.capture_turn_start(&turn(1)).unwrap();
    std::fs::remove_file(repo.path().join("doomed.txt")).unwrap();
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    assert_eq!(
        checkpoints.changed_files(&turn(1)).unwrap(),
        vec!["doomed.txt".to_string()]
    );
}

#[test]
fn a_turn_that_changed_nothing_reports_nothing() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    checkpoints.capture_turn_start(&turn(1)).unwrap();
    checkpoints.capture_turn_end(&turn(1)).unwrap();
    assert!(checkpoints.changed_files(&turn(1)).unwrap().is_empty());
    assert!(checkpoints.turn_patch(&turn(1)).unwrap().is_empty());
}

#[test]
fn the_turn_patch_shows_the_agents_work_and_not_the_hand_edit() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    repo.write("by-hand.txt", "mine\n");
    checkpoints.capture_turn_start(&turn(1)).unwrap();
    repo.write("by-agent.txt", "theirs\n");
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    let patch = checkpoints.turn_patch(&turn(1)).unwrap();
    assert!(patch.contains("by-agent.txt"), "{patch}");
    assert!(!patch.contains("by-hand.txt"), "{patch}");
}

#[test]
fn rewinding_puts_the_working_tree_back_to_where_the_turn_started() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    repo.write("README.md", "before the turn\n");
    checkpoints.capture_turn_start(&turn(1)).unwrap();

    repo.write("README.md", "the agent's version\n");
    repo.write("added-by-agent.txt", "new file\n");
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    checkpoints.rewind_to_turn_start(&turn(1)).unwrap();
    assert_eq!(repo.read("README.md").as_deref(), Some("before the turn\n"));
    assert_eq!(
        repo.read("added-by-agent.txt"),
        None,
        "a file the agent added after the checkpoint is not part of that state"
    );
}

#[test]
fn rewinding_leaves_the_branch_alone() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    checkpoints.capture_turn_start(&turn(1)).unwrap();
    repo.write("x.txt", "x\n");
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    let head_before = repo.head();
    checkpoints.rewind_to_turn_start(&turn(1)).unwrap();
    assert_eq!(repo.head(), head_before);
    assert_eq!(repo.git(["rev-parse", "--abbrev-ref", "HEAD"]), "main");
}

#[test]
fn a_branch_switched_between_turns_is_recorded_with_the_turn() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    let first = checkpoints.capture_turn_start(&turn(1)).unwrap();
    assert_eq!(first.branch.as_deref(), Some("main"));
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    repo.git(["checkout", "-q", "-b", "side"]);
    repo.write("side.txt", "side\n");
    repo.commit("side work");

    let second = checkpoints.capture_turn_start(&turn(2)).unwrap();
    assert_eq!(second.branch.as_deref(), Some("side"));
    assert_eq!(second.base_commit.as_deref(), Some(repo.head().as_str()));
    // The second turn is based on the side branch, not on where the first ran.
    assert_ne!(first.base_commit, second.base_commit);
}

#[test]
fn restarting_a_turn_replaces_its_checkpoint_rather_than_stacking_one() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    checkpoints.capture_turn_start(&turn(1)).unwrap();
    repo.write("first-attempt.txt", "a\n");
    let again = checkpoints.capture_turn_start(&turn(1)).unwrap();

    repo.write("second-attempt.txt", "b\n");
    checkpoints.capture_turn_end(&turn(1)).unwrap();

    assert!(again.tree.len() >= 40 || !again.tree.is_empty());
    assert_eq!(
        checkpoints.changed_files(&turn(1)).unwrap(),
        vec!["second-attempt.txt".to_string()],
        "the retry's starting state is the one that counts"
    );
}

#[test]
fn a_detached_head_records_no_branch_rather_than_the_word_head() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    repo.git(["checkout", "-q", "--detach"]);

    let start = checkpoints.capture_turn_start(&turn(1)).unwrap();
    assert_eq!(start.branch, None);
    assert_eq!(start.base_commit.as_deref(), Some(repo.head().as_str()));
}

#[test]
fn asking_about_a_turn_that_was_never_captured_is_not_an_error() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    assert!(!checkpoints.exists(&turn(9)).unwrap());
    assert!(checkpoints.changed_files(&turn(9)).unwrap().is_empty());
    assert!(checkpoints.rewind_to_turn_start(&turn(9)).is_err());
}

#[test]
fn dropping_a_session_removes_its_refs_and_leaves_others_alone() {
    let repo = TempRepo::new();
    let checkpoints = Checkpoints::new(repo.path());
    let other = TurnId::new("99999999-9999-9999-9999-999999999999", 1);
    checkpoints.capture_turn_start(&turn(1)).unwrap();
    checkpoints.capture_turn_start(&other).unwrap();

    checkpoints.forget_session(turn(1).session()).unwrap();
    assert!(!checkpoints.exists(&turn(1)).unwrap());
    assert!(checkpoints.exists(&other).unwrap());
}

#[test]
fn a_directory_that_is_not_a_repository_fails_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let checkpoints = Checkpoints::new(tmp.path());
    let error = checkpoints
        .capture_turn_start(&turn(1))
        .unwrap_err()
        .to_string();
    assert!(error.to_lowercase().contains("git"), "{error}");
}
