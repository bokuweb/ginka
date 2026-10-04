//! The client side of the reconnect contract.
//!
//! A reconnecting client asks for everything after the last sequence number it
//! saw, so the boundary event usually arrives twice; and a daemon that
//! restarted numbers from one again, so a cursor from the previous run points
//! at the wrong events entirely. Both are handled here rather than in a view,
//! because the CLI reconnects too and has to reach the same conclusions.

use ginka_protocol::envelope::Seq;

/// What the client should do with an event that just arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Render it.
    Deliver,
    /// Already seen — the overlap a resume always produces. Drop it.
    Duplicate,
    /// Something was missed. The client must re-read rather than render a
    /// transcript with a hole in it.
    Gap {
        /// The sequence number that should have come next.
        expected: Seq,
    },
    /// A different daemon run. Everything the client knows is from a stream
    /// that no longer exists.
    Resync,
}

/// How far through the event stream this client is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventCursor {
    epoch: Option<u64>,
    last_seq: Seq,
}

impl EventCursor {
    /// The last sequence number delivered; zero before the first event or
    /// after a resync.
    pub fn last_seq(&self) -> Seq {
        self.last_seq
    }

    /// The daemon run the cursor follows; `None` until the first event.
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
    }

    /// What to send as the resume point on the next connection.
    pub fn resume_from(&self) -> Seq {
        self.last_seq
    }

    /// Classify an arriving event and advance if it is the next one.
    ///
    /// The first event on a fresh cursor is taken as the baseline: a client
    /// reads the world and then follows the stream, so where the stream
    /// happens to be when it starts following is by definition where it is.
    pub fn accept(&mut self, epoch: u64, seq: Seq) -> Delivery {
        match self.epoch {
            None => {
                self.epoch = Some(epoch);
                self.last_seq = seq;
                Delivery::Deliver
            }
            Some(current) if current != epoch => {
                // Adopt the new run, but forget the position: sequence numbers
                // in this stream mean something else entirely.
                self.epoch = Some(epoch);
                self.last_seq = 0;
                Delivery::Resync
            }
            Some(_) if seq <= self.last_seq => Delivery::Duplicate,
            Some(_) if seq == self.last_seq + 1 => {
                self.last_seq = seq;
                Delivery::Deliver
            }
            Some(_) => Delivery::Gap {
                expected: self.last_seq + 1,
            },
        }
    }

    /// Forget the position, so the next connection re-reads the world. Called
    /// after a gap, and after the daemon answers a resume with one.
    pub fn resync(&mut self) {
        self.last_seq = 0;
    }
}
