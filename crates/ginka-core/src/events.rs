//! The server side of the reconnect contract.
//!
//! Every push carries a sequence number, and the daemon holds a bounded window
//! of recent events, so a client whose socket dropped can ask for what it
//! missed instead of re-reading the world. The window is bounded on purpose:
//! a client that was away long enough is told to resync, which is a far better
//! failure than an unbounded buffer or a transcript with a hole in it.
//! See `docs/roadmap.md` §5 (M2).

use ginka_protocol::envelope::Seq;
use std::collections::VecDeque;

/// What a client asking "everything after N" gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay<T> {
    /// The client has seen everything.
    UpToDate,
    /// What it missed, oldest first.
    Events(Vec<(Seq, T)>),
    /// It fell out of the window; it has to re-read the world. The bounds are
    /// carried so the client can log how far behind it was.
    Gap {
        /// The oldest sequence number still held, or `latest + 1` when none is.
        oldest: Seq,
        /// The newest sequence number published.
        latest: Seq,
    },
    /// The cursor is from the future — a stale cursor against a restarted
    /// daemon, or a corrupt one. Also a resync, but a different bug to chase.
    Reset,
}

/// Recent events, numbered from one.
#[derive(Debug, Clone)]
pub struct EventLog<T> {
    capacity: usize,
    next_seq: Seq,
    held: VecDeque<(Seq, T)>,
}

impl<T: Clone> EventLog<T> {
    /// An empty log holding at most `capacity` events; older ones fall out of
    /// the replay window. The first event gets sequence number 1.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            // Sequence numbers start at 1, so a cursor of 0 can mean "nothing
            // seen yet" without a sentinel.
            next_seq: 1,
            held: VecDeque::new(),
        }
    }

    /// Record an event and return the sequence number it was published under.
    pub fn append(&mut self, event: T) -> Seq {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.held.push_back((seq, event));
        while self.held.len() > self.capacity {
            self.held.pop_front();
        }
        seq
    }

    /// Sequence number of the last event published; 0 before the first.
    pub fn latest_seq(&self) -> Seq {
        self.next_seq - 1
    }

    /// Sequence number of the oldest event still held; `None` while empty.
    pub fn oldest_seq(&self) -> Option<Seq> {
        self.held.front().map(|(seq, _)| *seq)
    }

    /// Events currently held, at most the capacity.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// True when nothing is held: before the first append, or always with a
    /// capacity of zero.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Everything published after `after`.
    pub fn since(&self, after: Seq) -> Replay<T> {
        let latest = self.latest_seq();
        if after > latest {
            return Replay::Reset;
        }
        if after == latest {
            return Replay::UpToDate;
        }
        let Some(oldest) = self.oldest_seq() else {
            return Replay::Gap {
                oldest: latest + 1,
                latest,
            };
        };
        if after + 1 < oldest {
            return Replay::Gap { oldest, latest };
        }
        Replay::Events(
            self.held
                .iter()
                .filter(|(seq, _)| *seq > after)
                .cloned()
                .collect(),
        )
    }
}
