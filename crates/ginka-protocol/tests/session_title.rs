//! N5: a session carries two titles — one the user sets, one the agent
//! supplies — and the user's always wins.

use ginka_protocol::session::{DEFAULT_TITLE, SessionTitle};

#[test]
fn an_unnamed_session_shows_the_default() {
    let title = SessionTitle::default();
    assert_eq!(title.display(), DEFAULT_TITLE);
    assert!(title.wants_agent_title());
}

#[test]
fn the_users_title_beats_the_agents() {
    let mut title = SessionTitle::default();
    title.set_agent("Refactor the driver trait");
    title.set_user(Some("Driver work".into()));
    assert_eq!(title.display(), "Driver work");

    // A title arriving later must not overwrite what the user typed.
    title.set_agent("Something the model thought of");
    assert_eq!(title.display(), "Driver work");
}

#[test]
fn clearing_the_users_title_falls_back_to_the_agents() {
    let mut title = SessionTitle::default();
    title.set_agent("Refactor the driver trait");
    title.set_user(Some("Driver work".into()));
    title.set_user(None);
    assert_eq!(title.display(), "Refactor the driver trait");
}

#[test]
fn a_blank_user_title_counts_as_no_title() {
    let mut title = SessionTitle::default();
    title.set_agent("Refactor the driver trait");
    title.set_user(Some("   ".into()));
    assert_eq!(title.display(), "Refactor the driver trait");
}

#[test]
fn the_prompt_placeholder_is_replaced_silently_by_the_agents_title() {
    let mut title = SessionTitle::default();
    assert!(title.seed_from_prompt("add a steering capability to the driver trait"));
    assert_eq!(title.display(), "add a steering capability to the driver…");
    assert!(
        title.wants_agent_title(),
        "a placeholder still wants a real title"
    );

    title.set_agent("Add driver steering");
    assert_eq!(title.display(), "Add driver steering");
    assert!(!title.wants_agent_title());
}

#[test]
fn seeding_never_overwrites_a_real_title() {
    let mut title = SessionTitle::default();
    title.set_agent("Add driver steering");
    assert!(!title.seed_from_prompt("some later prompt"));
    assert_eq!(title.display(), "Add driver steering");
}

#[test]
fn seeding_twice_keeps_the_first_prompt() {
    let mut title = SessionTitle::default();
    assert!(title.seed_from_prompt("first prompt"));
    assert!(!title.seed_from_prompt("second prompt"));
    assert_eq!(title.display(), "first prompt");
}

#[test]
fn a_short_prompt_is_used_whole_without_an_ellipsis() {
    let mut title = SessionTitle::default();
    title.seed_from_prompt("  fix the flaky test\n");
    assert_eq!(title.display(), "fix the flaky test");
}

#[test]
fn a_multibyte_prompt_is_cut_on_a_character_boundary() {
    let mut title = SessionTitle::default();
    // No spaces at all, so the word budget cannot help: the char cap must.
    let prompt = "ドライバのステアリング機能を追加してテストも書いてくださいというながいぷろんぷとをここに置いて文字数の上限だけで切られることを確かめる";
    title.seed_from_prompt(prompt);
    let shown = title.display().to_string();
    assert!(shown.ends_with('…'), "{shown}");
    assert!(shown.chars().count() <= SessionTitle::MAX_PLACEHOLDER_CHARS + 1);
    assert!(prompt.starts_with(shown.trim_end_matches('…')));
}

#[test]
fn an_empty_prompt_seeds_nothing() {
    let mut title = SessionTitle::default();
    assert!(!title.seed_from_prompt("   \n "));
    assert_eq!(title.display(), DEFAULT_TITLE);
}

#[test]
fn it_round_trips_through_json() {
    let mut title = SessionTitle::default();
    title.seed_from_prompt("something");
    title.set_user(Some("Mine".into()));
    let json = serde_json::to_string(&title).unwrap();
    assert_eq!(serde_json::from_str::<SessionTitle>(&json).unwrap(), title);
}
