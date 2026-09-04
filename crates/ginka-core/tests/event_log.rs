//! The server side of the reconnect contract: every push carries a sequence
//! number, and a client that went away can ask for what it missed.

use ginka_core::events::{EventLog, Replay};

fn log_of(count: u64, capacity: usize) -> EventLog<String> {
    let mut log = EventLog::new(capacity);
    for n in 1..=count {
        log.append(format!("event {n}"));
    }
    log
}

#[test]
fn sequence_numbers_start_at_one_so_zero_can_mean_nothing_seen_yet() {
    let mut log = EventLog::<String>::new(16);
    assert_eq!(log.latest_seq(), 0);
    assert_eq!(log.append("first".into()), 1);
    assert_eq!(log.append("second".into()), 2);
    assert_eq!(log.latest_seq(), 2);
}

#[test]
fn a_fresh_client_asks_from_zero_and_gets_everything_held() {
    let log = log_of(3, 16);
    let Replay::Events(events) = log.since(0) else {
        panic!("expected a replay");
    };
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], (1, "event 1".to_string()));
    assert_eq!(events[2], (3, "event 3".to_string()));
}

#[test]
fn a_reconnecting_client_gets_exactly_what_it_missed() {
    let log = log_of(5, 16);
    let Replay::Events(events) = log.since(3) else {
        panic!("expected a replay");
    };
    assert_eq!(
        events.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
        [4, 5]
    );
}

#[test]
fn a_client_that_missed_nothing_is_told_so_rather_than_sent_an_empty_batch() {
    let log = log_of(5, 16);
    assert_eq!(log.since(5), Replay::UpToDate);
}

#[test]
fn a_client_that_fell_out_of_the_buffer_is_told_to_resync() {
    // The buffer is bounded, so a client away long enough cannot be caught up
    // from it. Saying so is the difference between a resync and a hole.
    let log = log_of(20, 5);
    match log.since(3) {
        Replay::Gap { oldest, latest } => {
            assert_eq!(oldest, 16);
            assert_eq!(latest, 20);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_oldest_event_still_held_is_replayable() {
    let log = log_of(20, 5);
    let Replay::Events(events) = log.since(15) else {
        panic!("15 is exactly one before the oldest held event");
    };
    assert_eq!(events.len(), 5);
    assert_eq!(events[0].0, 16);
}

#[test]
fn a_cursor_from_the_future_is_a_resync_not_a_panic() {
    // A stale cursor against a restarted daemon, or a corrupt one on disk.
    let log = log_of(3, 16);
    assert_eq!(log.since(99), Replay::Reset);
}

#[test]
fn the_buffer_never_grows_past_its_capacity() {
    let log = log_of(1_000, 8);
    assert_eq!(log.len(), 8);
    assert_eq!(log.latest_seq(), 1_000);
}

#[test]
fn an_empty_log_answers_every_cursor_without_replaying_anything() {
    let log = EventLog::<String>::new(4);
    assert_eq!(log.since(0), Replay::UpToDate);
    assert_eq!(log.since(9), Replay::Reset);
}

#[test]
fn a_zero_capacity_log_still_counts_and_reports_a_gap() {
    // Not a configuration we ship, but a bound of 0 must not divide by zero.
    let mut log = EventLog::<String>::new(0);
    assert_eq!(log.append("dropped".into()), 1);
    assert_eq!(log.len(), 0);
    assert!(matches!(log.since(0), Replay::Gap { .. }));
}
