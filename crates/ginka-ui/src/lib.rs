//! Design tokens, assets and view models.
//!
//! Everything the UI needs that is *not* a deeply nested render chain lives
//! here, so it can carry unit tests. The binary crate keeps only the views:
//! `rustc` overflows its stack expanding `#[test]` in a crate that also holds
//! the toolkit's builder chains, so the split is load-bearing, not cosmetic.

pub mod assets;
pub mod layout;
pub mod surface;
pub mod theme;
pub mod transcript;
pub mod workspace;

pub use assets::Assets;
pub use layout::{Layout, Panel};
pub use theme::{Mode, Tokens};
pub use transcript::{Block, Transcript};
