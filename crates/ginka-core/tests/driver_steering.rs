//! N1/N2: a follow-up typed while the agent is working goes *into* the running
//! turn where the transport can take it, and only falls back to a queue where
//! it cannot. Option changes ask the driver, except the ones policy decides.

use ginka_core::driver::{
    AgentSession, Dispatch, FollowUps, SessionPhase, apply_session_options,
    testing::ScriptedSession,
};
use ginka_protocol::provider::{AccessMode, OptionOutcome, SessionOptions};

#[test]
fn a_message_with_no_turn_running_starts_one() {
    let mut follow_ups = FollowUps::default();
    let dispatch = follow_ups.submit("do the thing", SessionPhase::Idle, true);
    assert_eq!(dispatch, Dispatch::StartTurn("do the thing".into()));
    assert!(follow_ups.pending().is_empty());
}

#[test]
fn a_message_during_a_turn_is_steered_in_when_the_transport_can_take_it() {
    let mut follow_ups = FollowUps::default();
    let dispatch = follow_ups.submit("actually, use serde_json", SessionPhase::Turn, true);
    assert_eq!(dispatch, Dispatch::Steer("actually, use serde_json".into()));
    assert!(follow_ups.pending().is_empty());
}

#[test]
fn a_transport_that_cannot_steer_queues_instead_and_shows_the_message() {
    let mut follow_ups = FollowUps::default();
    let dispatch = follow_ups.submit("and update the docs", SessionPhase::Turn, false);
    assert_eq!(dispatch, Dispatch::Queued);
    assert_eq!(follow_ups.pending(), ["and update the docs"]);
}

#[test]
fn a_message_sent_while_still_connecting_is_queued_even_if_steering_is_supported() {
    let mut follow_ups = FollowUps::default();
    assert_eq!(
        follow_ups.submit("early", SessionPhase::Connecting, true),
        Dispatch::Queued
    );
    assert_eq!(follow_ups.pending(), ["early"]);
}

#[test]
fn a_rejected_steer_becomes_the_next_turn_rather_than_being_lost() {
    let mut follow_ups = FollowUps::default();
    follow_ups.submit("actually, use serde_json", SessionPhase::Turn, true);
    follow_ups.steer_rejected("actually, use serde_json");
    assert_eq!(follow_ups.pending(), ["actually, use serde_json"]);

    assert_eq!(
        follow_ups.turn_finished(),
        Some(Dispatch::StartTurn("actually, use serde_json".into()))
    );
    assert!(follow_ups.pending().is_empty());
}

#[test]
fn a_rejected_steer_keeps_its_place_ahead_of_later_messages() {
    let mut follow_ups = FollowUps::default();
    follow_ups.submit("first", SessionPhase::Turn, true);
    follow_ups.submit("second", SessionPhase::Turn, false);
    follow_ups.steer_rejected("first");
    assert_eq!(follow_ups.pending(), ["first", "second"]);
}

#[test]
fn queued_messages_open_one_turn_together_when_the_current_one_settles() {
    let mut follow_ups = FollowUps::default();
    follow_ups.submit("and update the docs", SessionPhase::Turn, false);
    follow_ups.submit("and the changelog", SessionPhase::Turn, false);

    assert_eq!(
        follow_ups.turn_finished(),
        Some(Dispatch::StartTurn(
            "and update the docs\n\nand the changelog".into()
        ))
    );
    assert!(follow_ups.pending().is_empty());
    assert_eq!(follow_ups.turn_finished(), None);
}

#[test]
fn an_accepted_steer_leaves_nothing_pending() {
    let mut follow_ups = FollowUps::default();
    follow_ups.submit("actually, use serde_json", SessionPhase::Turn, true);
    follow_ups.steer_accepted();
    assert!(follow_ups.pending().is_empty());
    assert_eq!(follow_ups.turn_finished(), None);
}

#[test]
fn blank_input_is_never_dispatched() {
    let mut follow_ups = FollowUps::default();
    assert_eq!(
        follow_ups.submit("   ", SessionPhase::Idle, true),
        Dispatch::Nothing
    );
    assert_eq!(
        follow_ups.submit("\n", SessionPhase::Turn, false),
        Dispatch::Nothing
    );
    assert!(follow_ups.pending().is_empty());
}

fn options() -> SessionOptions {
    SessionOptions {
        model: Some("sonnet".into()),
        reasoning_effort: Some("medium".into()),
        service_tier: None,
        access_mode: AccessMode::Ask,
        account: None,
    }
}

#[test]
fn an_account_change_restarts_without_asking_the_driver() {
    // The vendor's thread lives in the account's directory, and a resume
    // cannot cross directories (`docs/accounts.md` §5).
    let mut session = ScriptedSession::new().absorbs_options(true);
    let mut current = options();
    let next = SessionOptions {
        account: Some(ginka_protocol::AccountId("claude-work".into())),
        ..options()
    };
    let outcome = apply_session_options(&mut session, &mut current, next).unwrap();
    assert_eq!(outcome, OptionOutcome::RestartRequired);
    assert_eq!(session.applied(), Vec::<SessionOptions>::new());
}

#[test]
fn an_unchanged_option_set_never_reaches_the_driver() {
    let mut session = ScriptedSession::new().steering(true);
    let mut current = options();
    let outcome = apply_session_options(&mut session, &mut current, options()).unwrap();
    assert_eq!(outcome, OptionOutcome::Absorbed);
    assert_eq!(session.applied(), Vec::<SessionOptions>::new());
}

#[test]
fn a_model_change_the_transport_absorbs_keeps_the_session() {
    let mut session = ScriptedSession::new().absorbs_options(true);
    let mut current = options();
    let next = SessionOptions {
        model: Some("opus".into()),
        ..options()
    };
    let outcome = apply_session_options(&mut session, &mut current, next.clone()).unwrap();
    assert_eq!(outcome, OptionOutcome::Absorbed);
    assert_eq!(session.applied(), vec![next.clone()]);
    assert_eq!(
        current, next,
        "the absorbed change is now the session's state"
    );
}

#[test]
fn a_model_change_the_transport_refuses_asks_for_a_restart() {
    let mut session = ScriptedSession::new().absorbs_options(false);
    let mut current = options();
    let next = SessionOptions {
        model: Some("opus".into()),
        ..options()
    };
    let outcome = apply_session_options(&mut session, &mut current, next.clone()).unwrap();
    assert_eq!(outcome, OptionOutcome::RestartRequired);
    assert_eq!(session.applied(), vec![next]);
    assert_eq!(
        current,
        options(),
        "nothing changed until the restart lands"
    );
}

#[test]
fn an_access_mode_change_restarts_without_asking_the_driver() {
    // Even a transport that would happily absorb it does not get to widen what
    // an already-running agent may touch.
    let mut session = ScriptedSession::new().absorbs_options(true);
    let mut current = options();
    let next = SessionOptions {
        access_mode: AccessMode::Auto,
        ..options()
    };
    let outcome = apply_session_options(&mut session, &mut current, next).unwrap();
    assert_eq!(outcome, OptionOutcome::RestartRequired);
    assert_eq!(
        session.applied(),
        Vec::<SessionOptions>::new(),
        "the driver was never asked"
    );
}

#[test]
fn a_driver_that_fails_to_apply_leaves_the_options_alone() {
    let mut session = ScriptedSession::new().failing_options();
    let mut current = options();
    let next = SessionOptions {
        model: Some("opus".into()),
        ..options()
    };
    assert!(apply_session_options(&mut session, &mut current, next).is_err());
    assert_eq!(current, options());
}

#[test]
fn a_session_reports_whether_it_can_steer() {
    assert!(ScriptedSession::new().steering(true).supports_steer());
    assert!(!ScriptedSession::new().steering(false).supports_steer());
}
