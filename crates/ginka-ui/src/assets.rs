//! Asset source.
//!
//! Ours first, the toolkit's second. `gpui-component`'s icon set is broad but
//! not exhaustive — it has no git branch and no paperclip, both of which the
//! design in `docs/ui.md` leans on, and no compose mark for "new chat" — so
//! app-specific icons live here and take precedence over a same-named toolkit
//! asset.

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
    (
        "icons/square-pen.svg",
        include_str!("../../../assets/icons/square-pen.svg"),
    ),
    (
        "icons/agent-cube.svg",
        include_str!("../../../assets/icons/agent-cube.svg"),
    ),
    (
        "icons/agent-orbit.svg",
        include_str!("../../../assets/icons/agent-orbit.svg"),
    ),
    (
        "icons/agent-spark.svg",
        include_str!("../../../assets/icons/agent-spark.svg"),
    ),
    (
        "icons/agent-prompt.svg",
        include_str!("../../../assets/icons/agent-prompt.svg"),
    ),
    (
        "icons/compass.svg",
        include_str!("../../../assets/icons/compass.svg"),
    ),
    (
        "icons/hammer.svg",
        include_str!("../../../assets/icons/hammer.svg"),
    ),
    (
        "icons/list-check.svg",
        include_str!("../../../assets/icons/list-check.svg"),
    ),
    (
        "icons/bug.svg",
        include_str!("../../../assets/icons/bug.svg"),
    ),
    (
        "icons/gauge.svg",
        include_str!("../../../assets/icons/gauge.svg"),
    ),
    (
        "icons/notebook.svg",
        include_str!("../../../assets/icons/notebook.svg"),
    ),
    (
        "icons/file-plus.svg",
        include_str!("../../../assets/icons/file-plus.svg"),
    ),
    (
        "icons/lock.svg",
        include_str!("../../../assets/icons/lock.svg"),
    ),
    (
        "icons/lock-open.svg",
        include_str!("../../../assets/icons/lock-open.svg"),
    ),
    (
        "icons/quote.svg",
        include_str!("../../../assets/icons/quote.svg"),
    ),
    (
        "icons/git-pull-request.svg",
        include_str!("../../../assets/icons/git-pull-request.svg"),
    ),
    (
        "icons/git-pull-request-draft.svg",
        include_str!("../../../assets/icons/git-pull-request-draft.svg"),
    ),
    (
        "icons/git-merge.svg",
        include_str!("../../../assets/icons/git-merge.svg"),
    ),
    (
        "icons/git-pull-request-closed.svg",
        include_str!("../../../assets/icons/git-pull-request-closed.svg"),
    ),
];

/// Our icons, addressed the way [`gpui_component::Icon::path`] expects.
pub mod icon {
    pub const GIT_BRANCH: &str = "icons/git-branch.svg";
    pub const PAPERCLIP: &str = "icons/paperclip.svg";
    /// The compose mark on "new chat": a page with a pen on it, which is what
    /// every other agent client uses for the same thing.
    pub const SQUARE_PEN: &str = "icons/square-pen.svg";
    pub const AGENT_CUBE: &str = "icons/agent-cube.svg";
    pub const AGENT_ORBIT: &str = "icons/agent-orbit.svg";
    pub const AGENT_SPARK: &str = "icons/agent-spark.svg";
    pub const AGENT_PROMPT: &str = "icons/agent-prompt.svg";
    /// The four marks on the home screen's starters, one per kind of first
    /// question: explore, build, review, fix.
    pub const COMPASS: &str = "icons/compass.svg";
    pub const HAMMER: &str = "icons/hammer.svg";
    pub const LIST_CHECK: &str = "icons/list-check.svg";
    pub const BUG: &str = "icons/bug.svg";
    /// The Reports surface: how close each login is to its wall.
    pub const GAUGE: &str = "icons/gauge.svg";
    /// Notes, in the project rail.
    pub const NOTEBOOK: &str = "icons/notebook.svg";
    /// A new note.
    pub const FILE_PLUS: &str = "icons/file-plus.svg";
    /// Access modes: a closed lock for asking first, an open one for none.
    pub const LOCK: &str = "icons/lock.svg";
    pub const LOCK_OPEN: &str = "icons/lock-open.svg";
    /// Quoting a message into the composer: a bar beside the lines it quotes.
    pub const QUOTE: &str = "icons/quote.svg";
    /// A workspace's pull request, one mark per state: open, draft, merged
    /// and closed.
    pub const GIT_PULL_REQUEST: &str = "icons/git-pull-request.svg";
    pub const GIT_PULL_REQUEST_DRAFT: &str = "icons/git-pull-request-draft.svg";
    pub const GIT_MERGE: &str = "icons/git-merge.svg";
    pub const GIT_PULL_REQUEST_CLOSED: &str = "icons/git-pull-request-closed.svg";
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
        for path in [
            icon::GIT_BRANCH,
            icon::PAPERCLIP,
            icon::AGENT_CUBE,
            icon::AGENT_ORBIT,
            icon::AGENT_SPARK,
            icon::AGENT_PROMPT,
            icon::COMPASS,
            icon::HAMMER,
            icon::LIST_CHECK,
            icon::BUG,
            icon::NOTEBOOK,
            icon::FILE_PLUS,
            icon::LOCK,
            icon::LOCK_OPEN,
            icon::QUOTE,
        ] {
            assert!(
                ICONS.iter().any(|(name, _)| *name == path),
                "{path} is not embedded"
            );
        }
    }
}
