//! The views.
//!
//! A library, so a host window can mount them (`docs/roadmap.md` E1): the
//! standalone binary mounts [`Shell`], which is the three-column window;
//! Ginka will mount the pieces under it — [`sidebar::Sidebar`],
//! [`list::ItemList`] and [`detail::Detail`] — over its own [`store::Store`].
//! Every view reaches GitHub through the store, and the store through an
//! `Arc<dyn GitHub>`, which is the whole of the network's surface (E2).
//!
//! No tests here, by construction: `rustc` overflows its stack expanding
//! `#[test]` next to the toolkit's builder chains (`AGENTS.md` rule 6).

rust_i18n::i18n!("../../locales", fallback = "en");

pub mod agent;
pub mod avatar;
pub mod browser;
pub mod detail;
pub mod fade;
pub mod history;
pub mod list;
pub mod palette;
pub mod shell;
pub mod sidebar;
pub mod signin;
pub mod skeleton;
pub mod store;

pub use shell::Shell;
pub use store::Store;

use gpui::App;

/// Bind the keys the views answer to. Call once, after `gpui_component::init`.
pub fn init(cx: &mut App) {
    shell::init(cx);
}
