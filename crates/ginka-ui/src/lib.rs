//! Design tokens, assets and view models.
//!
//! Everything the UI needs that is *not* a deeply nested render chain lives
//! here, so it can carry unit tests. The binary crate keeps only the views:
//! `rustc` overflows its stack expanding `#[test]` in a crate that also holds
//! the toolkit's builder chains, so the split is load-bearing, not cosmetic.

// The strings live in the workspace's own `locales/`, shared by every crate
// that shows one. English is the fallback, so a key a translator has not
// reached yet still renders as words.
rust_i18n::i18n!("../../locales", fallback = "en");

pub mod accounts;
pub mod assets;
pub mod branches;
pub mod browser;
pub mod composer;
pub mod dock;
pub mod editor;
pub mod fan_out;
pub mod field;
pub mod file_search;
pub mod file_tree;
pub mod graph;
pub mod handoff;
pub mod home;
pub mod layout;
pub mod markup;
pub mod models;
pub mod motion;
pub mod navigation;
pub mod notify;
pub mod palette;
pub mod reports;
pub mod search;
pub mod session_list;
pub mod skills;
pub mod split_diff;
pub mod surface;
pub mod tabs;
pub mod terminal;
pub mod theme;
pub mod transcript;
pub mod workspace;

pub use assets::Assets;
pub use layout::{Layout, Panel};
pub use theme::{Mode, Tokens};
pub use transcript::{
    Activity, Block, Reveal, Transcript, command_being_typed, complete_command, complete_mention,
    following, mention_being_typed, settled,
};
