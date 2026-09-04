//! N4: slash commands come from the provider as well as from disk, and the
//! `@file` index is bounded.

mod support;

use ginka_core::composer::{
    CommandScope, FILTER_CAP, FileIndex, SlashCommand, Trigger, filter_commands, filter_files,
    merge_commands,
};
use support::TempRepo;

#[test]
fn a_slash_at_the_start_of_the_composer_opens_the_command_list() {
    let trigger = Trigger::detect("/rev", 4).unwrap();
    assert_eq!(
        trigger,
        Trigger::Slash {
            query: "rev".into(),
            start: 0
        }
    );
}

#[test]
fn a_slash_inside_a_word_is_just_a_slash() {
    assert_eq!(Trigger::detect("see https://example.com", 23), None);
    assert_eq!(Trigger::detect("src/main.rs", 11), None);
}

#[test]
fn a_slash_on_a_later_line_still_opens_the_list() {
    let text = "some context\n/rev";
    assert_eq!(
        Trigger::detect(text, text.len()).unwrap(),
        Trigger::Slash {
            query: "rev".into(),
            start: 13
        }
    );
}

#[test]
fn an_at_sign_after_whitespace_opens_the_file_list() {
    let text = "look at @src/ma";
    assert_eq!(
        Trigger::detect(text, text.len()).unwrap(),
        Trigger::File {
            query: "src/ma".into(),
            start: 8
        }
    );
}

#[test]
fn an_at_sign_inside_a_word_is_not_a_mention() {
    // An email address is the reason this rule exists.
    assert_eq!(Trigger::detect("mail me@example.com", 19), None);
}

#[test]
fn nothing_triggers_once_the_cursor_has_moved_past_a_space() {
    assert_eq!(Trigger::detect("/review the diff", 16), None);
    assert_eq!(Trigger::detect("@src/main.rs and then", 21), None);
}

#[test]
fn the_trigger_follows_the_cursor_rather_than_the_end_of_the_text() {
    let text = "/rev and more";
    assert_eq!(
        Trigger::detect(text, 4).unwrap(),
        Trigger::Slash {
            query: "rev".into(),
            start: 0
        }
    );
}

#[test]
fn an_empty_query_still_opens_the_list() {
    assert_eq!(
        Trigger::detect("/", 1).unwrap(),
        Trigger::Slash {
            query: String::new(),
            start: 0
        }
    );
}

#[test]
fn a_provider_command_shadows_a_disk_command_of_the_same_name() {
    let from_provider = vec![
        SlashCommand::new("review", CommandScope::Provider)
            .with_description("ask the agent to review"),
    ];
    let from_disk = vec![
        SlashCommand::new("review", CommandScope::Project).with_description("stale copy"),
        SlashCommand::new("deploy", CommandScope::Project),
    ];

    let merged = merge_commands(from_provider, from_disk);
    assert_eq!(
        merged.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["deploy", "review"]
    );
    let review = merged.iter().find(|c| c.name == "review").unwrap();
    assert_eq!(review.scope, CommandScope::Provider);
    assert_eq!(
        review.description.as_deref(),
        Some("ask the agent to review")
    );
}

#[test]
fn filtering_puts_the_closest_match_first() {
    let commands = vec![
        SlashCommand::new("review-diff", CommandScope::Project),
        SlashCommand::new("rev", CommandScope::Provider),
        SlashCommand::new("deploy", CommandScope::Project),
    ];
    let matched = filter_commands(&commands, "rev");
    assert_eq!(matched[0].name, "rev");
    assert_eq!(matched.len(), 2, "deploy does not match");
}

#[test]
fn filtering_by_nothing_returns_everything_in_order() {
    let commands = vec![
        SlashCommand::new("deploy", CommandScope::Project),
        SlashCommand::new("review", CommandScope::Provider),
    ];
    assert_eq!(filter_commands(&commands, "").len(), 2);
}

#[test]
fn a_filter_never_returns_more_than_a_screenful() {
    let commands: Vec<SlashCommand> = (0..FILTER_CAP * 2)
        .map(|n| SlashCommand::new(format!("command-{n}"), CommandScope::Project))
        .collect();
    assert_eq!(filter_commands(&commands, "command").len(), FILTER_CAP);
}

#[test]
fn the_file_index_comes_from_git_and_skips_what_git_ignores() {
    let repo = TempRepo::new();
    repo.write(".gitignore", "target/\n");
    repo.write("src/main.rs", "fn main() {}\n");
    repo.commit("add sources");
    repo.write("untracked.md", "new\n");
    repo.write("target/build.log", "noise\n");

    let index = FileIndex::build(repo.path(), 1_000).unwrap();
    assert!(index.paths().iter().any(|p| p == "src/main.rs"));
    assert!(
        index.paths().iter().any(|p| p == "untracked.md"),
        "a file the agent just created is mentionable before it is committed"
    );
    assert!(!index.paths().iter().any(|p| p.starts_with("target/")));
    assert!(!index.truncated());
}

#[test]
fn the_file_index_is_bounded_and_says_so() {
    let repo = TempRepo::new();
    for n in 0..20 {
        repo.write(&format!("file-{n}.txt"), "x\n");
    }
    let index = FileIndex::build(repo.path(), 5).unwrap();
    assert_eq!(index.paths().len(), 5);
    assert!(
        index.truncated(),
        "a generated monorepo must degrade to best-effort, not hang"
    );
}

#[test]
fn a_plain_folder_still_produces_an_index() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("notes")).unwrap();
    std::fs::write(tmp.path().join("notes/todo.md"), "x").unwrap();
    std::fs::write(tmp.path().join("readme.txt"), "x").unwrap();

    let index = FileIndex::build(tmp.path(), 1_000).unwrap();
    assert!(index.paths().iter().any(|p| p == "notes/todo.md"));
    assert!(index.paths().iter().any(|p| p == "readme.txt"));
}

#[test]
fn filtering_files_matches_on_the_whole_path() {
    let paths = vec![
        "src/driver/mod.rs".to_string(),
        "src/review.rs".to_string(),
        "README.md".to_string(),
    ];
    let matched = filter_files(&paths, "drivermod");
    assert_eq!(matched, ["src/driver/mod.rs"]);
}
