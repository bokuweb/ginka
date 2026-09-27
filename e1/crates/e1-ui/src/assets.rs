//! Asset source.
//!
//! Ours first, the toolkit's second. `gpui-component`'s icon set has no pull
//! request, merge or issue mark — the three glyphs this app is made of — so
//! app-specific icons live here and take precedence over a same-named toolkit
//! asset.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// The asset source the window is opened with.
pub struct Assets;

/// Icons this app ships. Paths match what `Icon::path` is given.
const ICONS: &[(&str, &str)] = &[
    (
        icon::PULL_REQUEST,
        include_str!("../../../assets/icons/git-pull-request.svg"),
    ),
    (
        icon::PULL_REQUEST_DRAFT,
        include_str!("../../../assets/icons/git-pull-request-draft.svg"),
    ),
    (
        icon::PULL_REQUEST_CLOSED,
        include_str!("../../../assets/icons/git-pull-request-closed.svg"),
    ),
    (
        icon::MERGE,
        include_str!("../../../assets/icons/git-merge.svg"),
    ),
    (
        icon::CIRCLE_DOT,
        include_str!("../../../assets/icons/circle-dot.svg"),
    ),
    (
        icon::MESSAGE,
        include_str!("../../../assets/icons/message-square.svg"),
    ),
    (icon::LOCK, include_str!("../../../assets/icons/lock.svg")),
    (
        icon::PROJECT,
        include_str!("../../../assets/icons/project.svg"),
    ),
    (
        icon::USER_CHECK,
        include_str!("../../../assets/icons/user-check.svg"),
    ),
    (icon::E1, include_str!("../../../assets/icons/e1.svg")),
];

/// Our icons, addressed the way [`gpui_component::Icon::path`] expects.
pub mod icon {
    /// An open pull request.
    pub const PULL_REQUEST: &str = "icons/git-pull-request.svg";
    /// A draft pull request.
    pub const PULL_REQUEST_DRAFT: &str = "icons/git-pull-request-draft.svg";
    /// A pull request closed without merging.
    pub const PULL_REQUEST_CLOSED: &str = "icons/git-pull-request-closed.svg";
    /// A merged pull request.
    pub const MERGE: &str = "icons/git-merge.svg";
    /// An open issue.
    pub const CIRCLE_DOT: &str = "icons/circle-dot.svg";
    /// A comment count.
    pub const MESSAGE: &str = "icons/message-square.svg";
    /// A private repository.
    pub const LOCK: &str = "icons/lock.svg";
    /// A GitHub Project.
    pub const PROJECT: &str = "icons/project.svg";
    /// Assigned to you.
    pub const USER_CHECK: &str = "icons/user-check.svg";
    /// The app's own mark: an `E` and a `1`, painted in [`crate::Tokens::logo`].
    pub const E1: &str = "icons/e1.svg";
    /// The toolkit's: the light theme.
    pub const SUN: &str = "icons/sun.svg";
    /// The toolkit's: the dark theme.
    pub const MOON: &str = "icons/moon.svg";
    /// The toolkit's: a closed issue.
    pub const CIRCLE_CHECK: &str = "icons/circle-check.svg";
    /// The toolkit's: the inbox.
    pub const INBOX: &str = "icons/inbox.svg";
    /// The toolkit's: a review request.
    pub const EYE: &str = "icons/eye.svg";
    /// The toolkit's: a release or anything else in the inbox.
    pub const BELL: &str = "icons/bell.svg";
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
    fn our_icons_load() {
        for path in [
            icon::PULL_REQUEST,
            icon::PULL_REQUEST_DRAFT,
            icon::PULL_REQUEST_CLOSED,
            icon::MERGE,
            icon::CIRCLE_DOT,
            icon::MESSAGE,
            icon::LOCK,
            icon::PROJECT,
            icon::USER_CHECK,
            icon::E1,
        ] {
            let loaded = Assets
                .load(path)
                .unwrap()
                .unwrap_or_else(|| panic!("{path} is not embedded"));
            assert!(String::from_utf8_lossy(&loaded).contains("<svg"), "{path}");
        }
    }

    #[test]
    fn the_toolkit_icons_we_lean_on_are_really_there() {
        for path in [
            icon::CIRCLE_CHECK,
            icon::INBOX,
            icon::EYE,
            icon::BELL,
            icon::SUN,
            icon::MOON,
        ] {
            assert!(Assets.load(path).unwrap().is_some(), "{path}");
        }
    }
}
