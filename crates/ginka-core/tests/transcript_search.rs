//! N10: transcript search is answered by the daemon, over stored messages.

use ginka_core::db;
use ginka_core::transcript::{
    self, MessageRole, NewMessage, SEARCH_RESULT_CAP, SearchScope, Session,
};
use ginka_protocol::provider::ProviderKind;
use ginka_protocol::session::SessionTitle;
use rusqlite::Connection;

fn session(id: &str) -> Session {
    Session {
        id: id.into(),
        workspace: "comet/bright-harbor".into(),
        provider: ProviderKind::Claude,
        title: SessionTitle::default(),
    }
}

fn seeded() -> Connection {
    let conn = db::open_in_memory().unwrap();
    transcript::upsert_session(&conn, &session("a")).unwrap();
    transcript::upsert_session(&conn, &session("b")).unwrap();
    conn
}

fn say(conn: &Connection, session: &str, role: MessageRole, text: &str) -> i64 {
    transcript::append_message(
        conn,
        &NewMessage {
            session: session.into(),
            role,
            text: text.into(),
        },
    )
    .unwrap()
}

#[test]
fn messages_come_back_in_the_order_they_were_written() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "first");
    say(&conn, "a", MessageRole::Agent, "second");
    say(&conn, "a", MessageRole::User, "third");

    let messages = transcript::messages(&conn, "a", 10, None).unwrap();
    let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["first", "second", "third"]);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Agent);
}

#[test]
fn a_transcript_pages_backwards_from_the_end() {
    let conn = seeded();
    for n in 0..10 {
        say(&conn, "a", MessageRole::User, &format!("message {n}"));
    }

    // The newest page first: this is what a transcript opens on.
    let last = transcript::messages(&conn, "a", 3, None).unwrap();
    assert_eq!(
        last.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["message 7", "message 8", "message 9"]
    );

    let earlier = transcript::messages(&conn, "a", 3, Some(last[0].seq)).unwrap();
    assert_eq!(
        earlier.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["message 4", "message 5", "message 6"]
    );
}

#[test]
fn search_is_case_insensitive_and_crosses_sessions() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "Add a Steering capability");
    say(&conn, "b", MessageRole::Agent, "steering is now supported");

    let hits = transcript::search(&conn, "STEERING", SearchScope::Everywhere).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().any(|hit| hit.session == "a"));
    assert!(hits.iter().any(|hit| hit.session == "b"));
}

#[test]
fn search_can_be_scoped_to_one_session() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "steering");
    say(&conn, "b", MessageRole::User, "steering");

    let hits = transcript::search(&conn, "steering", SearchScope::Session("b".into())).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session, "b");
}

#[test]
fn the_newest_match_is_first() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "steering once");
    say(&conn, "a", MessageRole::User, "steering twice");

    let hits = transcript::search(&conn, "steering", SearchScope::Everywhere).unwrap();
    assert_eq!(hits[0].text, "steering twice");
}

#[test]
fn a_wildcard_in_the_query_is_matched_literally() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "100% coverage");
    say(&conn, "a", MessageRole::User, "nothing to see here");

    // Without escaping, "%" would match every message ever written.
    let hits = transcript::search(&conn, "100%", SearchScope::Everywhere).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].text, "100% coverage");

    let underscores = transcript::search(&conn, "a_b", SearchScope::Everywhere).unwrap();
    assert!(underscores.is_empty(), "_ is a wildcard too");
}

#[test]
fn an_empty_query_finds_nothing_rather_than_everything() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "something");
    assert!(
        transcript::search(&conn, "   ", SearchScope::Everywhere)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_hit_carries_a_snippet_centred_on_the_match() {
    let conn = seeded();
    let long = format!(
        "{} the needle {}",
        "padding ".repeat(40),
        "trailing ".repeat(40)
    );
    say(&conn, "a", MessageRole::User, &long);

    let hits = transcript::search(&conn, "needle", SearchScope::Everywhere).unwrap();
    let snippet = &hits[0].snippet;
    assert!(snippet.contains("needle"), "{snippet}");
    assert!(snippet.len() < long.len());
    assert!(
        snippet.starts_with('…') && snippet.ends_with('…'),
        "{snippet}"
    );
}

#[test]
fn a_snippet_never_splits_a_character() {
    let conn = seeded();
    let text = format!("{}ステアリング機能{}", "あ".repeat(80), "い".repeat(80));
    say(&conn, "a", MessageRole::User, &text);

    let hits = transcript::search(&conn, "ステアリング", SearchScope::Everywhere).unwrap();
    // Reaching this line at all means the snippet was cut on a boundary; a
    // byte-wise cut would have panicked while slicing.
    assert!(hits[0].snippet.contains("ステアリング"));
}

#[test]
fn results_are_capped() {
    let conn = seeded();
    for _ in 0..(SEARCH_RESULT_CAP + 10) {
        say(&conn, "a", MessageRole::User, "steering");
    }
    let hits = transcript::search(&conn, "steering", SearchScope::Everywhere).unwrap();
    assert_eq!(hits.len(), SEARCH_RESULT_CAP);
}

#[test]
fn a_session_title_survives_a_round_trip() {
    let conn = db::open_in_memory().unwrap();
    let mut stored = session("a");
    stored.title.seed_from_prompt("add a steering capability");
    stored.title.set_user(Some("Steering".into()));
    transcript::upsert_session(&conn, &stored).unwrap();

    let loaded = transcript::session(&conn, "a").unwrap().unwrap();
    assert_eq!(loaded, stored);
    assert_eq!(loaded.title.display(), "Steering");
}

#[test]
fn re_upserting_a_session_updates_it_rather_than_duplicating() {
    let conn = seeded();
    let mut updated = session("a");
    updated.title.set_agent("Renamed by the agent");
    transcript::upsert_session(&conn, &updated).unwrap();

    assert_eq!(transcript::sessions(&conn).unwrap().len(), 2);
    assert_eq!(
        transcript::session(&conn, "a")
            .unwrap()
            .unwrap()
            .title
            .display(),
        "Renamed by the agent"
    );
}

#[test]
fn deleting_a_session_takes_its_messages_with_it() {
    let conn = seeded();
    say(&conn, "a", MessageRole::User, "steering");
    say(&conn, "b", MessageRole::User, "steering");

    transcript::delete_session(&conn, "a").unwrap();
    let hits = transcript::search(&conn, "steering", SearchScope::Everywhere).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session, "b");
    assert!(transcript::session(&conn, "a").unwrap().is_none());
}
