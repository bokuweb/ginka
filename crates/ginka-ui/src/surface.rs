//! What the right panel can show: `docs/ui.md` §3.4.

use crate::assets::icon;
use gpui_component::{Icon, IconName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Terminal,
    Git,
    Files,
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
        Surface::Reports,
        Surface::Skills,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Git => "Git",
            Self::Files => "Files",
            Self::Reports => "Reports",
            Self::Skills => "Skills",
        }
    }

    /// Stable lowercase value stored in per-workspace app settings.
    pub fn key(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Git => "git",
            Self::Files => "files",
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

    pub fn icon(self) -> Icon {
        match self {
            Self::Terminal => Icon::new(IconName::SquareTerminal),
            Self::Git => Icon::empty().path(icon::GIT_BRANCH),
            Self::Files => Icon::new(IconName::Folder),
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
            Self::Reports => rust_i18n::t!("surface.reports.pending").to_string(),
            Self::Skills => rust_i18n::t!("surface.skills.pending").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_surface_is_offered_and_labelled() {
        assert_eq!(Surface::ALL.len(), 5);
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
}
