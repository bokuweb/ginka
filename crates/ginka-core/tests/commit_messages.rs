//! N9: a commit subject is a fixed classification over a diff that is already
//! in the prompt, so it runs on a cheap tier and a bounded diff.

mod support;

use ginka_core::commit::{self, CommitMessage, MAX_DIFF_BYTES};
use ginka_protocol::provider::{ProviderKind, SessionOptions};
use support::TempRepo;

#[test]
fn generation_pins_a_cheap_tier_whatever_the_session_runs_on() {
    let session = SessionOptions {
        model: Some("the-most-expensive-model".into()),
        reasoning_effort: Some("high".into()),
        ..SessionOptions::default()
    };

    for provider in ProviderKind::ALL {
        let choice = commit::message_model(provider, &session);
        assert_ne!(
            choice.model.as_deref(),
            Some("the-most-expensive-model"),
            "{provider} must not bill the session's model for a subject line"
        );
        assert_ne!(choice.reasoning_effort.as_deref(), Some("high"));
    }
}

#[test]
fn the_choice_is_stable_for_a_provider() {
    let a = commit::message_model(ProviderKind::Claude, &SessionOptions::default());
    let b = commit::message_model(ProviderKind::Claude, &SessionOptions::default());
    assert_eq!(a, b);
}

#[test]
fn the_prompt_carries_the_diff_and_the_file_list() {
    let prompt = commit::build_prompt(&["src/main.rs".into(), "README.md".into()], "diff --git …");
    assert!(prompt.contains("src/main.rs"));
    assert!(prompt.contains("README.md"));
    assert!(prompt.contains("diff --git"));
}

#[test]
fn an_enormous_diff_is_cut_and_the_prompt_admits_it() {
    let diff = "+".repeat(MAX_DIFF_BYTES * 2);
    let prompt = commit::build_prompt(&["generated.rs".into()], &diff);

    assert!(prompt.len() < diff.len(), "the cap has to actually bite");
    assert!(
        prompt.contains("truncated"),
        "a silent cut would mislead the model"
    );
    // The file list survives the cut: it is the part that is always useful.
    assert!(prompt.contains("generated.rs"));
}

#[test]
fn a_plain_answer_becomes_the_subject() {
    let message = CommitMessage::parse("Add driver steering").unwrap();
    assert_eq!(message.subject, "Add driver steering");
    assert!(message.body.is_none());
}

#[test]
fn a_wrapped_or_decorated_answer_is_unwrapped() {
    for raw in [
        "\"Add driver steering\"",
        "`Add driver steering`",
        "```\nAdd driver steering\n```",
        "Subject: Add driver steering",
        "  Add driver steering  \n",
    ] {
        assert_eq!(
            CommitMessage::parse(raw).unwrap().subject,
            "Add driver steering",
            "{raw:?}"
        );
    }
}

#[test]
fn a_subject_and_body_are_kept_apart() {
    let message =
        CommitMessage::parse("Add driver steering\n\nThe queue is the fallback.\n").unwrap();
    assert_eq!(message.subject, "Add driver steering");
    assert_eq!(message.body.as_deref(), Some("The queue is the fallback."));
}

#[test]
fn a_conventional_prefix_is_left_alone() {
    assert_eq!(
        CommitMessage::parse("feat(driver): add steering")
            .unwrap()
            .subject,
        "feat(driver): add steering"
    );
}

#[test]
fn a_trailing_period_is_removed_but_an_ellipsis_is_not() {
    assert_eq!(
        CommitMessage::parse("Add driver steering.")
            .unwrap()
            .subject,
        "Add driver steering"
    );
    assert_eq!(
        CommitMessage::parse("Add driver steering…")
            .unwrap()
            .subject,
        "Add driver steering…"
    );
}

#[test]
fn an_overlong_subject_is_cut_on_a_word() {
    let raw =
        "Add a very long and rambling description of what this change does to the driver layer";
    let subject = CommitMessage::parse(raw).unwrap().subject;
    assert!(subject.chars().count() <= CommitMessage::MAX_SUBJECT_CHARS);
    assert!(raw.starts_with(subject.trim_end_matches('…')));
}

#[test]
fn an_empty_answer_is_an_error_rather_than_an_empty_commit_message() {
    assert!(CommitMessage::parse("   \n\n").is_err());
    assert!(CommitMessage::parse("```\n```").is_err());
}

#[test]
fn staging_and_committing_writes_the_message_verbatim() {
    let repo = TempRepo::new();
    repo.write("added.txt", "hello\n");

    let message = CommitMessage::parse("Add a file\n\nBecause the test needs one.").unwrap();
    commit::commit_all(repo.path(), &message).unwrap();

    assert_eq!(repo.git(["log", "-1", "--pretty=%s"]), "Add a file");
    assert_eq!(
        repo.git(["log", "-1", "--pretty=%b"]).trim(),
        "Because the test needs one."
    );
    assert_eq!(repo.git(["status", "--porcelain"]), "");
}

#[test]
fn committing_a_clean_worktree_fails_clearly() {
    let repo = TempRepo::new();
    let message = CommitMessage::parse("Nothing to do").unwrap();
    let error = commit::commit_all(repo.path(), &message)
        .unwrap_err()
        .to_string();
    assert!(error.contains("nothing to commit"), "{error}");
}
