//! Design tokens, assets, settings and view models.
//!
//! Everything the UI needs that is *not* a deeply nested render chain lives
//! here, so it can carry unit tests. `e1-views` keeps only the views:
//! `rustc` overflows its stack expanding `#[test]` in a crate that also holds
//! the toolkit's builder chains, so the split is load-bearing, not cosmetic
//! (`AGENTS.md` rule 6).

// The strings live in the workspace's own `locales/`, shared by every crate
// that shows one. English is the fallback, so a key a translator has not
// reached yet still renders as words.
rust_i18n::i18n!("../../locales", fallback = "en");

pub mod agents;
pub mod assets;
pub mod code;
pub mod diff;
pub mod fetch;
pub mod finder;
pub mod graph;
pub mod i18n;
pub mod layout;
pub mod log;
pub mod logging;
pub mod nav;
pub mod palette;
pub mod paths;
pub mod project;
pub mod rows;
pub mod settings;
pub mod snapshot;
pub mod theme;
pub mod time;
pub mod tree;

pub use assets::Assets;
pub use fetch::Fetch;
pub use layout::{HEADER_HEIGHT, Layout, Panel, SidebarResizeLimits, TRAFFIC_LIGHT_INSET};
pub use nav::{Focus, OwnerGroup, RepoTab, Section, group_by_owner};
pub use paths::Paths;
pub use rows::{Glyph, ItemRow, LabelChip, Role};
pub use settings::{AppSettings, Appearance};
pub use snapshot::{ItemDetail, Snapshot};
pub use theme::{Mode, Tokens};
