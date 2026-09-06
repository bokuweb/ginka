//! The Ginka daemon.
//!
//! Owns SQLite, agent processes, PTYs and git, and outlives the UI so agents
//! keep running when the window closes. The app and the CLI reach it over an
//! authenticated WebSocket on loopback; what they can ask for is
//! `ginka-protocol`'s `Request`, and what answers is `ginka-core`'s `Service`.
//!
//! This crate is only the transport around that service: framing, sequencing,
//! authentication and the discovery file. Anything that decides what a request
//! *means* belongs in `ginka-core` (`AGENTS.md` rule 2).

pub mod connectors;
pub mod handshake;
pub mod hub;
pub mod server;

pub use hub::{Hub, Sequenced, Subscription};
pub use server::Daemon;
