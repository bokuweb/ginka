//! Ginka's domain layer.
//!
//! Everything here must be testable without opening a window: the GPUI app is
//! a viewport onto this crate, and the daemon is its long-running host. If a
//! piece of logic needs a `Window` to be exercised, it is in the wrong crate.

pub mod attachment;
pub mod blob;
pub mod checkpoint;
pub mod commit;
pub mod composer;
pub mod db;
pub mod driver;
pub mod git;
pub mod logging;
pub mod paths;
pub mod project;
pub mod registry;
pub mod review;
pub mod settings;
pub mod skills;
pub mod transcript;
pub mod usage;

pub use paths::Paths;
