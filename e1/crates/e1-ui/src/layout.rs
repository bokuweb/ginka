//! Which columns are open, and how wide they are.
//!
//! Each panel toggles independently, a closed panel remembers the width it
//! had, and the arrangement survives a restart. The centre column is not a
//! panel — it can never be closed, so there is no arrangement that leaves the
//! window empty.

use crate::settings::AppSettings;
use gpui::{Pixels, px};

/// How tall the strip across the top of each column is.
///
/// There is no window-wide title bar (`docs/ui.md` §3.1): each column paints
/// itself to the top of the window and carries its own controls. The strips
/// share this height so their contents sit on one line.
pub const HEADER_HEIGHT: Pixels = px(44.);

/// What macOS reserves at the leading edge for the traffic lights.
///
/// Whichever column is leftmost has to leave this much room before its own
/// controls start, so it moves from the sidebar to the centre column when the
/// sidebar is closed.
pub const TRAFFIC_LIGHT_INSET: Pixels = px(78.);

/// The columns the user can open and close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// Navigation, on the left.
    Sidebar,
    /// The item being read, on the right.
    RightPanel,
    /// The CLI-backed chat, at the far right.
    AgentPanel,
}

/// Floors and ceilings applied while the sidebar divider is dragged.
#[derive(Debug, Clone, Copy)]
pub struct SidebarResizeLimits {
    /// The centre column's minimum width.
    pub centre_min: Pixels,
    /// The sidebar's minimum width.
    pub sidebar_min: Pixels,
    /// The sidebar's maximum width.
    pub sidebar_max: Pixels,
    /// The right panel's minimum width.
    pub right_min: Pixels,
}

impl Panel {
    /// Every optional panel, in render order.
    pub const ALL: &'static [Panel] = &[Panel::Sidebar, Panel::RightPanel, Panel::AgentPanel];

    /// The locale key for the panel's name.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Sidebar => "panel.sidebar",
            Self::RightPanel => "panel.right",
            Self::AgentPanel => "panel.agent",
        }
    }
}

/// The arrangement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    sidebar_open: bool,
    right_open: bool,
    agent_open: bool,
    sidebar_width: f32,
    right_width: f32,
    agent_width: f32,
}

impl Layout {
    /// Read the arrangement out of the settings.
    pub fn from_settings(settings: &AppSettings) -> Self {
        Self {
            sidebar_open: settings.sidebar_open,
            right_open: settings.right_panel_open,
            agent_open: settings.agent_panel_open,
            sidebar_width: settings.sidebar_width,
            right_width: settings.right_panel_width,
            agent_width: settings.agent_panel_width,
        }
    }

    /// Write it back, leaving the other fields untouched.
    pub fn write_into(&self, settings: &mut AppSettings) {
        settings.sidebar_open = self.sidebar_open;
        settings.right_panel_open = self.right_open;
        settings.agent_panel_open = self.agent_open;
        settings.sidebar_width = self.sidebar_width;
        settings.right_panel_width = self.right_width;
        settings.agent_panel_width = self.agent_width;
    }

    /// Whether a panel is showing.
    pub fn is_open(&self, panel: Panel) -> bool {
        match panel {
            Panel::Sidebar => self.sidebar_open,
            Panel::RightPanel => self.right_open,
            Panel::AgentPanel => self.agent_open,
        }
    }

    /// Flip a panel.
    pub fn toggle(&mut self, panel: Panel) {
        self.set_open(panel, !self.is_open(panel));
    }

    /// Open or close a panel.
    pub fn set_open(&mut self, panel: Panel, open: bool) {
        match panel {
            Panel::Sidebar => self.sidebar_open = open,
            Panel::RightPanel => self.right_open = open,
            Panel::AgentPanel => self.agent_open = open,
        }
    }

    /// Record a drag-resize. Kept while the panel is closed, so reopening
    /// restores what the user had rather than a default.
    pub fn set_size(&mut self, panel: Panel, size: Pixels) {
        let value = f32::from(size);
        match panel {
            Panel::Sidebar => self.sidebar_width = value,
            Panel::RightPanel => self.right_width = value,
            Panel::AgentPanel => self.agent_width = value,
        }
    }

    /// What occupies each slot of the horizontal group, in render order.
    ///
    /// `None` is the centre column, which has no stored size. Closing a panel
    /// shifts every slot after it, so the resize callback must consult this
    /// rather than assume index 0 is the sidebar.
    pub fn columns(&self) -> Vec<Option<Panel>> {
        let mut slots = Vec::with_capacity(4);
        if self.sidebar_open {
            slots.push(Some(Panel::Sidebar));
        }
        slots.push(None);
        if self.right_open {
            slots.push(Some(Panel::RightPanel));
        }
        if self.agent_open {
            slots.push(Some(Panel::AgentPanel));
        }
        slots
    }

    /// Record the sizes a drag produced, ignoring the centre column.
    ///
    /// Extra sizes are ignored rather than trusted: a mismatch means the
    /// toolkit's state and our slot map disagree, and guessing would write
    /// one panel's size over another's.
    pub fn record_sizes(&mut self, slots: &[Option<Panel>], sizes: &[Pixels]) {
        for (slot, size) in slots.iter().zip(sizes) {
            if let Some(panel) = slot {
                self.set_size(*panel, *size);
            }
        }
    }

    /// A panel's width.
    pub fn size(&self, panel: Panel) -> Pixels {
        px(match panel {
            Panel::Sidebar => self.sidebar_width,
            Panel::RightPanel => self.right_width,
            Panel::AgentPanel => self.agent_width,
        })
    }

    /// Shrink open optional columns until the centre and far-right agent fit.
    ///
    /// Settings written in a wider window can otherwise restore a panel
    /// completely beyond the trailing edge. The reading pane gives up space
    /// first, then the sidebar, while the agent keeps a usable composer.
    pub fn fit_to_viewport(
        &mut self,
        viewport: Pixels,
        centre_min: Pixels,
        sidebar_min: Pixels,
        right_min: Pixels,
        agent_min: Pixels,
    ) {
        let total = Panel::ALL
            .iter()
            .copied()
            .filter(|panel| self.is_open(*panel))
            .fold(centre_min, |total, panel| total + self.size(panel));
        let mut excess = (total - viewport).max(px(0.));

        for (panel, floor) in [
            (Panel::RightPanel, right_min),
            (Panel::Sidebar, sidebar_min),
            (Panel::AgentPanel, agent_min),
        ] {
            if excess <= px(0.) || !self.is_open(panel) {
                continue;
            }
            let available = (self.size(panel) - floor).max(px(0.));
            let shrink = excess.min(available);
            self.set_size(panel, self.size(panel) - shrink);
            excess -= shrink;
        }
    }

    /// Resize the sidebar from a drag's starting arrangement.
    ///
    /// The centre gives up space first. Once it reaches its floor, an open
    /// right panel gives up its remaining space down to its own floor. Using
    /// the starting arrangement makes reversing the drag restore both widths
    /// instead of treating the already-shrunk right panel as a new baseline.
    pub fn resize_sidebar_from(
        &mut self,
        start: Self,
        wanted: Pixels,
        viewport: Pixels,
        limits: SidebarResizeLimits,
    ) {
        let agent = if start.agent_open {
            start.size(Panel::AgentPanel)
        } else {
            px(0.)
        };
        let right_floor = if start.right_open {
            limits.right_min
        } else {
            px(0.)
        };
        let room = (viewport - limits.centre_min - agent).max(limits.sidebar_min + right_floor);
        let sidebar = wanted
            .max(limits.sidebar_min)
            .min(limits.sidebar_max)
            .min(room - right_floor);
        self.set_size(Panel::Sidebar, sidebar);

        if start.right_open {
            let right = start
                .size(Panel::RightPanel)
                .min((room - sidebar).max(limits.right_min));
            self.set_size(Panel::RightPanel, right);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESIZE_LIMITS: SidebarResizeLimits = SidebarResizeLimits {
        centre_min: px(320.),
        sidebar_min: px(200.),
        sidebar_max: px(480.),
        right_min: px(280.),
    };

    #[test]
    fn round_trips_through_settings() {
        let mut settings = AppSettings::default();
        let mut layout = Layout::from_settings(&settings);
        layout.toggle(Panel::RightPanel);
        layout.set_size(Panel::Sidebar, px(320.));
        layout.write_into(&mut settings);
        let restored = Layout::from_settings(&settings);
        assert_eq!(restored, layout);
        assert!(!restored.is_open(Panel::RightPanel));
        assert_eq!(restored.size(Panel::Sidebar), px(320.));
    }

    #[test]
    fn a_closed_panel_keeps_its_size() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        layout.set_size(Panel::Sidebar, px(320.));
        layout.toggle(Panel::Sidebar);
        layout.toggle(Panel::Sidebar);
        assert_eq!(layout.size(Panel::Sidebar), px(320.));
    }

    #[test]
    fn slots_track_which_panels_are_open() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        assert_eq!(
            layout.columns(),
            vec![Some(Panel::Sidebar), None, Some(Panel::RightPanel)]
        );
        layout.set_open(Panel::Sidebar, false);
        assert_eq!(layout.columns(), vec![None, Some(Panel::RightPanel)]);
    }

    #[test]
    fn the_agent_is_always_the_last_panel() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        layout.set_open(Panel::AgentPanel, true);
        assert_eq!(
            layout.columns(),
            vec![
                Some(Panel::Sidebar),
                None,
                Some(Panel::RightPanel),
                Some(Panel::AgentPanel),
            ]
        );
        layout.set_open(Panel::RightPanel, false);
        assert_eq!(
            layout.columns(),
            vec![Some(Panel::Sidebar), None, Some(Panel::AgentPanel)]
        );
    }

    #[test]
    fn a_resize_with_the_sidebar_closed_does_not_write_the_centre_width_into_it() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        let original = layout.size(Panel::Sidebar);
        layout.set_open(Panel::Sidebar, false);
        layout.record_sizes(&layout.columns(), &[px(900.), px(500.)]);
        assert_eq!(layout.size(Panel::Sidebar), original);
        assert_eq!(layout.size(Panel::RightPanel), px(500.));
    }

    #[test]
    fn record_sizes_ignores_a_length_mismatch_rather_than_guessing() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        let right = layout.size(Panel::RightPanel);
        layout.record_sizes(&layout.columns(), &[px(300.), px(900.)]);
        assert_eq!(layout.size(Panel::Sidebar), px(300.));
        assert_eq!(layout.size(Panel::RightPanel), right);
    }

    #[test]
    fn sidebar_takes_the_right_panels_spare_width_after_the_centre_reaches_its_floor() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        let start = layout;

        layout.resize_sidebar_from(start, px(350.), px(1_000.), RESIZE_LIMITS);

        assert_eq!(layout.size(Panel::Sidebar), px(350.));
        assert_eq!(layout.size(Panel::RightPanel), px(330.));
    }

    #[test]
    fn reversing_a_sidebar_drag_restores_the_right_panels_starting_width() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        let start = layout;
        layout.resize_sidebar_from(start, px(350.), px(1_000.), RESIZE_LIMITS);
        layout.resize_sidebar_from(start, px(250.), px(1_000.), RESIZE_LIMITS);

        assert_eq!(layout.size(Panel::Sidebar), px(250.));
        assert_eq!(layout.size(Panel::RightPanel), px(420.));
    }

    #[test]
    fn restored_wide_reading_pane_makes_room_for_the_agent() {
        let settings = AppSettings {
            sidebar_width: 209.0,
            right_panel_width: 911.0,
            agent_panel_open: true,
            agent_panel_width: 420.0,
            ..AppSettings::default()
        };
        let mut layout = Layout::from_settings(&settings);

        layout.fit_to_viewport(px(1_483.), px(320.), px(200.), px(280.), px(320.));

        assert_eq!(layout.size(Panel::RightPanel), px(534.));
        assert_eq!(layout.size(Panel::AgentPanel), px(420.));
        assert!(layout.is_open(Panel::AgentPanel));
    }
}
