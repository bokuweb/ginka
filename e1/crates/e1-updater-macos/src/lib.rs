//! Sparkle integration for the standalone macOS application.
//!
//! The crate is intentionally absent from `e1-views`: when those views are
//! mounted in Ginka they must not be able to update their host application.
//! Objective-C interop is contained here and the public API remains safe.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::Updater;
