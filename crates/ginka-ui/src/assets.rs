//! Asset source.
//!
//! Ours first, the toolkit's second. `gpui-component`'s icon set is broad but
//! not exhaustive — it has no git branch and no paperclip, both of which the
//! design in `docs/ui.md` leans on — so app-specific icons live here and take
//! precedence over a same-named toolkit asset.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

pub struct Assets;

/// Icons this app ships. Paths match what `Icon::path` is given.
const ICONS: &[(&str, &str)] = &[
    (
        "icons/git-branch.svg",
        include_str!("../../../assets/icons/git-branch.svg"),
    ),
    (
        "icons/paperclip.svg",
        include_str!("../../../assets/icons/paperclip.svg"),
    ),
];

/// Our icons, addressed the way [`gpui_component::Icon::path`] expects.
pub mod icon {
    pub const GIT_BRANCH: &str = "icons/git-branch.svg";
    pub const PAPERCLIP: &str = "icons/paperclip.svg";
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, contents)) = ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(contents.as_bytes())));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut listed: Vec<SharedString> = ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect();
        listed.extend(gpui_component_assets::Assets.list(path)?);
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_icons_take_precedence_and_load() {
        let loaded = Assets
            .load(icon::GIT_BRANCH)
            .unwrap()
            .expect("icon present");
        assert!(String::from_utf8_lossy(&loaded).contains("<svg"));
    }

    #[test]
    fn unknown_paths_fall_through_to_the_toolkit() {
        // The toolkit ships this one; we do not.
        assert!(Assets.load("icons/plus.svg").unwrap().is_some());
    }

    #[test]
    fn every_declared_icon_constant_resolves() {
        for path in [icon::GIT_BRANCH, icon::PAPERCLIP] {
            assert!(
                ICONS.iter().any(|(name, _)| *name == path),
                "{path} is not embedded"
            );
        }
    }
}
