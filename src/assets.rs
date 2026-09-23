//! The window's asset source.
//!
//! A GPUI app has exactly one, and with the `github` feature this window
//! mounts the GitHub client as its Inbox (`docs/ui.md` §3.5). Ours first, then
//! e1's, and the toolkit's behind both — the chain is what lets `e1-views` be
//! mounted without either crate knowing the other ships icons. Built without
//! the feature there is only ours, and this is a straight delegation.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        #[cfg(not(feature = "github"))]
        return ginka_ui::Assets.load(path);

        // A miss reaches the toolkit's own source, which reports it as an error
        // rather than as `Ok(None)` — so anything but a hit here means "not
        // ours", and the GitHub app's source, which asks the toolkit again, is
        // the one whose answer stands.
        #[cfg(feature = "github")]
        match ginka_ui::Assets.load(path) {
            Ok(Some(found)) => Ok(Some(found)),
            _ => e1_ui::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        #[cfg_attr(not(feature = "github"), allow(unused_mut))]
        let mut listed = ginka_ui::Assets.list(path)?;
        #[cfg(feature = "github")]
        for name in e1_ui::Assets.list(path)? {
            if !listed.contains(&name) {
                listed.push(name);
            }
        }
        Ok(listed)
    }
}
