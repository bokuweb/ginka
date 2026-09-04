//! The one event stream every driver normalizes into.
//!
//! [`AgentEvent`] itself lives in the protocol crate: it is persisted in
//! transcripts and pushed to every client, so neither the daemon nor a client
//! owns its shape. What lives here is the error a driver reports when a
//! vendor's output stops matching this build.

pub use ginka_protocol::event::AgentEvent;

/// What can go wrong reading a provider's stream.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// The agent wrote something that is not a message at all — a panic, a
    /// warning on stdout, a truncated line.
    #[error("the agent produced a line that is not JSON: {line}")]
    Malformed { line: String },
    /// A message we recognise, shaped in a way we do not. Names the field so
    /// the report says which part of the contract moved.
    #[error(
        "the agent's {shape} message has no {field}; this build may be too old \
         for the installed CLI"
    )]
    MissingField { shape: String, field: &'static str },
}

impl DriverError {
    /// Lines are put in the message, so bound them: a runaway process can emit
    /// a megabyte on one line and it would otherwise all land in the log.
    pub const MAX_REPORTED_LINE: usize = 300;

    pub fn malformed(line: &str) -> Self {
        let line = line.trim();
        let mut cut = Self::MAX_REPORTED_LINE.min(line.len());
        while cut > 0 && !line.is_char_boundary(cut) {
            cut -= 1;
        }
        Self::Malformed {
            line: line[..cut].to_string(),
        }
    }
}
