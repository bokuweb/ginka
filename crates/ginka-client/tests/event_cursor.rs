//! The client side of the reconnect contract: replayed events are deduplicated,
//! a hole is detected rather than rendered, and a restarted daemon invalidates
//! the cursor instead of silently continuing.

use ginka_client::cursor::{Delivery, EventCursor};

const EPOCH: u64 = 1_788_480_000;

#[test]
fn a_fresh_cursor_has_seen_nothing() {
    let cursor = EventCursor::default();
    assert_eq!(cursor.last_seq(), 0);
    assert_eq!(cursor.resume_from(), 0);
}

#[test]
fn events_in_order_are_delivered_and_advance_the_cursor() {
    let mut cursor = EventCursor::default();
    assert_eq!(cursor.accept(EPOCH, 1), Delivery::Deliver);
    assert_eq!(cursor.accept(EPOCH, 2), Delivery::Deliver);
    assert_eq!(cursor.last_seq(), 2);
}

#[test]
fn a_replayed_event_is_dropped_rather_than_shown_twice() {
    let mut cursor = EventCursor::default();
    cursor.accept(EPOCH, 1);
    cursor.accept(EPOCH, 2);

    // Reconnect: the daemon replays from the last seq the client asked for,
    // and the boundary event arrives again.
    assert_eq!(cursor.accept(EPOCH, 2), Delivery::Duplicate);
    assert_eq!(cursor.accept(EPOCH, 3), Delivery::Deliver);
    assert_eq!(cursor.last_seq(), 3);
}

#[test]
fn a_hole_is_reported_and_the_cursor_does_not_move_past_it() {
    let mut cursor = EventCursor::default();
    cursor.accept(EPOCH, 1);

    assert_eq!(cursor.accept(EPOCH, 5), Delivery::Gap { expected: 2 });
    assert_eq!(
        cursor.last_seq(),
        1,
        "advancing here would render a transcript with a hole in it"
    );
}

#[test]
fn a_restarted_daemon_invalidates_the_cursor() {
    let mut cursor = EventCursor::default();
    cursor.accept(EPOCH, 7);

    // A new run numbers from one again; continuing would drop everything.
    assert_eq!(cursor.accept(EPOCH + 1, 1), Delivery::Resync);
    assert_eq!(cursor.last_seq(), 0);
    assert_eq!(cursor.epoch(), Some(EPOCH + 1));
}

#[test]
fn after_a_resync_the_new_run_is_followed_normally() {
    let mut cursor = EventCursor::default();
    cursor.accept(EPOCH, 7);
    cursor.accept(EPOCH + 1, 1);

    assert_eq!(cursor.accept(EPOCH + 1, 1), Delivery::Deliver);
    assert_eq!(cursor.accept(EPOCH + 1, 2), Delivery::Deliver);
}

#[test]
fn resuming_asks_for_everything_after_what_was_seen() {
    let mut cursor = EventCursor::default();
    cursor.accept(EPOCH, 1);
    cursor.accept(EPOCH, 2);
    assert_eq!(cursor.resume_from(), 2);
}

#[test]
fn a_resync_forgets_the_cursor_so_the_client_re_reads_the_world() {
    let mut cursor = EventCursor::default();
    cursor.accept(EPOCH, 4);
    cursor.resync();
    assert_eq!(cursor.resume_from(), 0);
}
