//! Ginka's domain layer.
//!
//! Everything here must be testable without opening a window: the GPUI app is
//! a viewport onto this crate, and the daemon is its long-running host. If a
//! piece of logic needs a `Window` to be exercised, it is in the wrong crate.

pub mod agent;
pub mod checkpoint;
pub mod db;
pub mod diff;
pub mod driver;
pub mod git;
pub mod i18n;
pub mod logging;
pub mod paths;
pub mod project;
pub mod registry;
pub mod service;
pub mod session;
pub mod settings;

pub use paths::Paths;
