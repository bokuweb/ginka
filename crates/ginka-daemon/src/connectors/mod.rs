//! Chat connectors hosted in the daemon.
//!
//! The decisions live in `ginka-core::connector`; what is here is the glue
//! that gives them a platform and a daemon: the Slack adapter, and the
//! runner that feeds messages through the service and carries the agent's
//! events back to the thread (`docs/connectors.md` §2).

pub mod runner;
pub mod slack;

pub use runner::SlackConnector;
