//! Fan-out of daemon events to connected clients.
//!
//! Every event gets a monotonic sequence number and is kept in a bounded
//! replay window, so a client that drops its connection can ask for the gap
//! instead of re-reading the world. The window is what makes that bounded: a
//! client whose cursor has fallen out of it is told so, and re-reads.

use ginka_core::service::EventSink;
use ginka_protocol::{DaemonEvent, Seq};
use std::collections::VecDeque;
use std::sync::Mutex;

/// One event with the position it was published at.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequenced {
    pub seq: Seq,
    pub event: DaemonEvent,
}

/// Why a resume could not be answered from the replay window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// The oldest sequence number the hub can still replay.
    pub oldest: Seq,
}

/// A client's live view of the event stream.
///
/// Dropping it unsubscribes; the hub notices on its next publish rather than
/// keeping a dead sender around.
pub struct Subscription {
    receiver: async_channel::Receiver<Sequenced>,
}

impl Subscription {
    /// The next event, or `None` once the hub is gone.
    pub async fn next(&self) -> Option<Sequenced> {
        self.receiver.recv().await.ok()
    }

    /// Take the next event without waiting.
    pub fn try_next(&self) -> Option<Sequenced> {
        self.receiver.try_recv().ok()
    }
}

/// How many events a client may fall behind before it is disconnected.
///
/// A client that stops reading must not be able to grow the daemon's memory,
/// and it has a cheaper way back: reconnect and resume.
const SUBSCRIBER_BACKLOG: usize = 1024;

/// The event bus every connection subscribes to.
pub struct Hub {
    inner: Mutex<Inner>,
}

struct Inner {
    next_seq: Seq,
    window: usize,
    recent: VecDeque<Sequenced>,
    subscribers: Vec<async_channel::Sender<Sequenced>>,
}

impl Hub {
    /// Build a hub that can replay the last `window` events.
    pub fn new(window: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_seq: 1,
                window,
                recent: VecDeque::new(),
                subscribers: Vec::new(),
            }),
        }
    }

    /// The position a client with no stored cursor starts from: it wants what
    /// happens next, not the backlog of a daemon that has run for a week.
    pub fn current_seq(&self) -> Seq {
        self.lock().next_seq - 1
    }

    /// Subscribe to everything published from now on.
    pub fn subscribe(&self) -> Subscription {
        let (sender, receiver) = async_channel::bounded(SUBSCRIBER_BACKLOG);
        self.lock().subscribers.push(sender);
        Subscription { receiver }
    }

    /// Everything published after `after`.
    ///
    /// `Err(Gap)` means the client missed events that have already left the
    /// window, and has to re-read rather than patch.
    pub fn replay_after(&self, after: Seq) -> Result<Vec<Sequenced>, Gap> {
        let inner = self.lock();
        match inner.recent.front() {
            // Nothing has been published, or the client is already current.
            None => Ok(Vec::new()),
            Some(oldest) => {
                if after + 1 < oldest.seq {
                    return Err(Gap { oldest: oldest.seq });
                }
                Ok(inner
                    .recent
                    .iter()
                    .filter(|entry| entry.seq > after)
                    .cloned()
                    .collect())
            }
        }
    }

    /// How many clients are currently subscribed. For logging and tests.
    pub fn subscriber_count(&self) -> usize {
        self.lock().subscribers.len()
    }

    /// Close every subscription, letting what is already queued be delivered.
    ///
    /// This is how a shutdown reaches the clients in the right order: a closed
    /// `async_channel` still drains, so a client's last event — and the answer
    /// to the request that asked for the shutdown — goes out before its
    /// connection ends.
    pub fn close(&self) {
        let mut inner = self.lock();
        for sender in inner.subscribers.drain(..) {
            sender.close();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned hub means a panic while publishing. The event bus has no
        // invariants a panic can break, so recovering beats taking the daemon
        // down with it.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl EventSink for Hub {
    fn emit(&self, event: DaemonEvent) {
        let mut inner = self.lock();
        let seq = inner.next_seq;
        inner.next_seq += 1;
        let entry = Sequenced { seq, event };

        let window = inner.window;
        inner.recent.push_back(entry.clone());
        while inner.recent.len() > window {
            inner.recent.pop_front();
        }

        // `try_send` rather than `send`: publishing happens on the request
        // path, and one client that has stopped reading must not stall the
        // daemon for everyone else.
        inner
            .subscribers
            .retain(|sender| sender.try_send(entry.clone()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::ProjectName;

    fn event(name: &str) -> DaemonEvent {
        DaemonEvent::WorkspacesChanged {
            project: ProjectName(name.into()),
        }
    }

    #[test]
    fn events_are_numbered_from_one() {
        let hub = Hub::new(8);
        assert_eq!(hub.current_seq(), 0, "nothing has happened yet");
        hub.emit(event("a"));
        hub.emit(event("b"));
        assert_eq!(hub.current_seq(), 2);
    }

    #[test]
    fn a_subscriber_receives_what_is_published_after_it_subscribes() {
        let hub = Hub::new(8);
        hub.emit(event("before"));
        let subscription = hub.subscribe();
        hub.emit(event("after"));

        let received = subscription.try_next().expect("one event");
        assert_eq!(received.seq, 2);
        assert_eq!(received.event, event("after"));
        assert!(subscription.try_next().is_none());
    }

    #[test]
    fn replay_returns_only_what_the_client_has_not_seen() {
        let hub = Hub::new(8);
        for name in ["a", "b", "c"] {
            hub.emit(event(name));
        }
        let replayed = hub.replay_after(1).expect("within the window");
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].seq, 2);
        assert_eq!(replayed[1].seq, 3);

        assert!(hub.replay_after(3).unwrap().is_empty(), "already current");
    }

    #[test]
    fn a_cursor_older_than_the_window_reports_a_gap_rather_than_a_short_replay() {
        // Silently replaying "everything we still have" would leave the client
        // believing it is caught up when it is missing the events in between.
        let hub = Hub::new(2);
        for name in ["a", "b", "c", "d"] {
            hub.emit(event(name));
        }
        let gap = hub.replay_after(1).expect_err("1 has left the window");
        assert_eq!(gap.oldest, 3);
        // The oldest cursor still answerable is one before the oldest entry.
        assert_eq!(hub.replay_after(2).unwrap().len(), 2);
    }

    #[test]
    fn a_fresh_client_resumes_from_the_current_position_and_gets_nothing() {
        let hub = Hub::new(8);
        for name in ["a", "b"] {
            hub.emit(event(name));
        }
        assert!(hub.replay_after(hub.current_seq()).unwrap().is_empty());
    }

    #[test]
    fn closing_lets_what_is_queued_be_read_before_the_stream_ends() {
        // A shutdown has to reach a client *after* the answer to the request
        // that asked for it, so closing must drain rather than drop.
        let hub = Hub::new(8);
        let subscription = hub.subscribe();
        hub.emit(event("last"));
        hub.close();

        assert_eq!(
            subscription.try_next().map(|entry| entry.event),
            Some(event("last"))
        );
        assert!(subscription.try_next().is_none(), "and then it is over");
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn a_dropped_subscription_is_forgotten_on_the_next_publish() {
        let hub = Hub::new(8);
        let subscription = hub.subscribe();
        assert_eq!(hub.subscriber_count(), 1);
        drop(subscription);
        hub.emit(event("a"));
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn a_client_that_stops_reading_is_dropped_rather_than_growing_the_daemon() {
        let hub = Hub::new(4);
        let _subscription = hub.subscribe();
        for index in 0..SUBSCRIBER_BACKLOG + 8 {
            hub.emit(event(&index.to_string()));
        }
        assert_eq!(
            hub.subscriber_count(),
            0,
            "a client that never reads must be disconnected, not buffered"
        );
    }

    #[test]
    fn the_replay_window_is_bounded() {
        let hub = Hub::new(3);
        for index in 0..100 {
            hub.emit(event(&index.to_string()));
        }
        assert_eq!(hub.replay_after(97).unwrap().len(), 3);
    }
}
