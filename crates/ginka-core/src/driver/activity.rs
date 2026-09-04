//! The one shape a tool call collapses into.
//!
//! Defined in the protocol crate because it is persisted in transcripts and
//! rendered by every client; re-exported here because the drivers that build
//! it live in this module.

pub use ginka_protocol::event::{ActivityItem, ActivityKind};
