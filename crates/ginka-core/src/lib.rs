//! Ginka's domain layer.
//!
//! Everything here must be testable without opening a window: the GPUI app is
//! a viewport onto this crate, and the daemon is its long-running host. If a
//! piece of logic needs a `Window` to be exercised, it is in the wrong crate.

pub mod account;
pub mod agent;
pub mod attachment;
pub mod blob;
pub mod browser;
pub mod browser_history;
pub mod checkpoint;
pub mod chrome_cookies;
pub mod cli_sessions;
pub mod commands;
pub mod comments;
pub mod commit;
pub mod composer;
pub mod conflicts;
pub mod connector;
pub mod crash;
pub mod cron;
pub mod daemon;
pub mod db;
pub mod diff;
pub mod driver;
pub mod events;
pub mod files;
pub mod git;
pub mod handoff;
pub mod i18n;
pub mod logging;
pub mod lsp;
pub mod mcp;
pub mod notes;
pub mod paths;
pub mod project;
pub mod quick_commands;
pub mod registry;
pub mod review;
pub mod service;
pub mod session;
pub mod settings;
pub mod setup;
pub mod skills;
pub mod terminal;
pub mod tickets;
pub mod tool_path;
pub mod tools;
pub mod usage;
pub mod worktree_include;

pub use paths::Paths;
