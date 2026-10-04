//! What the right panel can show: `docs/ui.md` §3.4.

use crate::assets::icon;
use gpui_component::{Icon, IconName};

/// One kind of tab the right panel's dock can hold; the arrangement persists per workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The workspace's daemon terminals, drawn here instead of in the dock while this tab is on screen.
    Terminal,
    /// Status, diff, staging, commit, history and pull-request checks.
    Git,
    /// The file tree, project search and editor tabs.
    Files,
    /// The selected workspace's embedded browser and design feedback tools.
    Browser,
    /// What the work cost, and how close each login is to its wall
    /// (`docs/accounts.md` §11).
    Reports,
    /// The reusable instructions installed for coding agents.
    Skills,
}

impl Surface {
    /// Every surface the chooser offers, in the order it offers them.
    pub const ALL: &'static [Surface] = &[
        Surface::Terminal,
        Surface::Git,
        Surface::Files,
        Surface::Browser,
        Surface::Reports,
        Surface::Skills,
    ];

    /// The surface's stable English name, used as an element id; what the
    /// reader sees is [`Surface::title`].
    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Git => "Git",
            Self::Files => "Files",
            Self::Browser => "Browser",
            Self::Reports => "Reports",
            Self::Skills => "Skills",
        }
    }

    /// The surface's name in its tab and the chooser, in the window's
    /// language.
    pub fn title(self) -> String {
        self.title_in(&rust_i18n::locale())
    }

    /// [`Surface::title`] in a given locale.
    pub fn title_in(self, locale: &str) -> String {
        rust_i18n::t!(format!("surface.name.{}", self.key()), locale = locale).to_string()
    }

    /// Stable lowercase value stored in per-workspace app settings.
    pub fn key(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Git => "git",
            Self::Files => "files",
            Self::Browser => "browser",
            Self::Reports => "reports",
            Self::Skills => "skills",
        }
    }

    /// Restore a stored key, ignoring surfaces a newer build may have added.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|surface| surface.key() == key)
    }

    /// The name the dock area saves this surface's panel under.
    pub fn panel_name(self) -> &'static str {
        match self {
            Self::Terminal => "GinkaSurface.terminal",
            Self::Git => "GinkaSurface.git",
            Self::Files => "GinkaSurface.files",
            Self::Browser => "GinkaSurface.browser",
            Self::Reports => "GinkaSurface.reports",
            Self::Skills => "GinkaSurface.skills",
        }
    }

    /// The surface a dock panel name stands for.
    pub fn from_panel_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|surface| surface.panel_name() == name)
    }

    /// The surface after `current`, wrapping to the first surface.
    pub fn next(current: Option<Self>) -> Self {
        let index = current
            .and_then(|current| Self::ALL.iter().position(|surface| *surface == current))
            .map_or(0, |index| (index + 1) % Self::ALL.len());
        Self::ALL[index]
    }

    /// The surface before `current`, wrapping to the last surface.
    pub fn previous(current: Option<Self>) -> Self {
        let index = current
            .and_then(|current| Self::ALL.iter().position(|surface| *surface == current))
            .map_or(Self::ALL.len() - 1, |index| {
                (index + Self::ALL.len() - 1) % Self::ALL.len()
            });
        Self::ALL[index]
    }

    /// The mark in the surface's tab and on its chooser button.
    pub fn icon(self) -> Icon {
        match self {
            Self::Terminal => Icon::new(IconName::SquareTerminal),
            Self::Git => Icon::empty().path(icon::GIT_BRANCH),
            Self::Files => Icon::new(IconName::Folder),
            Self::Browser => Icon::new(IconName::Globe),
            Self::Reports => Icon::empty().path(icon::GAUGE),
            Self::Skills => Icon::empty().path(icon::LIST_CHECK),
        }
    }

    /// Shown in the placeholder until the surface is implemented.
    ///
    /// The labels above are proper nouns and stay as they are; this is a
    /// sentence, so it is translated.
    pub fn availability(self) -> String {
        match self {
            Self::Terminal => rust_i18n::t!("surface.terminal.pending").to_string(),
            Self::Git => rust_i18n::t!("surface.git.pending").to_string(),
            Self::Files => rust_i18n::t!("surface.files.pending").to_string(),
            Self::Browser => rust_i18n::t!("surface.browser.pending").to_string(),
            Self::Reports => rust_i18n::t!("surface.reports.pending").to_string(),
            Self::Skills => rust_i18n::t!("surface.skills.pending").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_surface_has_a_title_in_each_language() {
        for surface in Surface::ALL {
            let key = format!("surface.name.{}", surface.key());
            for locale in ["en", "ja"] {
                let title = surface.title_in(locale);
                assert!(!title.is_empty() && title != key, "{key} in {locale}");
            }
        }
        assert_eq!(Surface::Files.title_in("ja"), "ファイル");
    }

    #[test]
    fn every_surface_is_offered_and_labelled() {
        assert_eq!(Surface::ALL.len(), 6);
        for surface in Surface::ALL {
            assert!(!surface.label().is_empty());
            assert!(!surface.availability().is_empty());
        }
    }

    #[test]
    fn stored_surface_keys_round_trip_and_unknown_values_are_ignored() {
        for surface in Surface::ALL {
            assert_eq!(Surface::from_key(surface.key()), Some(*surface));
        }
        assert_eq!(Surface::from_key("future-surface"), None);
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        assert_eq!(Surface::next(Some(Surface::Terminal)), Surface::Git);
        assert_eq!(Surface::next(Some(Surface::Skills)), Surface::Terminal);
        assert_eq!(Surface::previous(Some(Surface::Terminal)), Surface::Skills);
        assert_eq!(Surface::previous(Some(Surface::Git)), Surface::Terminal);
    }

    #[test]
    fn cycling_an_empty_panel_starts_at_the_nearest_end() {
        assert_eq!(Surface::next(None), Surface::Terminal);
        assert_eq!(Surface::previous(None), Surface::Skills);
    }
}
