//! What the right panel can show: `docs/ui.md` §3.4.

use crate::assets::icon;
use gpui_component::{Icon, IconName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Terminal,
    Git,
}

impl Surface {
    /// Every surface the chooser offers, in the order it offers them.
    pub const ALL: &'static [Surface] = &[Surface::Terminal, Surface::Git];

    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Git => "Git",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Terminal => Icon::new(IconName::SquareTerminal),
            Self::Git => Icon::empty().path(icon::GIT_BRANCH),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_surface_is_offered_and_labelled() {
        assert_eq!(Surface::ALL.len(), 2);
        for surface in Surface::ALL {
            assert!(!surface.label().is_empty());
            assert!(!surface.availability().is_empty());
        }
    }
}
