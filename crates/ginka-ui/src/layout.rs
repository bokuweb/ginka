//! Which panels are open, and how big they are.
//!
//! Modelled on VS Code: each panel toggles independently, a closed panel
//! remembers the size it had, and the state survives a restart. The centre
//! column is not a panel — it can never be closed, so there is no arrangement
//! that leaves the window empty.

use ginka_core::settings::{AppSettings, WorkspaceLayoutSettings};
use ginka_protocol::WorkspaceId;
use gpui::{Pixels, px};

/// How tall the strip across the top of each column is.
///
/// There is no window-wide title bar (`docs/ui.md` §3.1): each column paints
/// itself to the top of the window and carries its own controls, which is what
/// makes the window read as one surface rather than as a bar over a layout.
/// The three strips share this height so their contents sit on one line.
pub const HEADER_HEIGHT: Pixels = px(44.);

/// What macOS reserves at the leading edge for the traffic lights.
///
/// Whichever column is leftmost has to leave this much room before its own
/// controls start, so it moves from the sidebar to the centre column when the
/// sidebar is closed.
pub const TRAFFIC_LIGHT_INSET: Pixels = px(78.);

/// Width of the always-present project navigation rail.
pub const PROJECT_RAIL_WIDTH: Pixels = px(188.);

/// Width of the navigator for its current selection state.
///
/// The session column is contextual: before a project is selected the rail
/// stands alone and the new-session surface gets the remaining room.
pub fn navigator_width(project_selected: bool, expanded: Pixels) -> Pixels {
    if project_selected {
        expanded
    } else {
        PROJECT_RAIL_WIDTH
    }
}

/// The panels the user can open and close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    /// The session list on the left.
    Sidebar,
    /// The surfaces column on the right.
    RightPanel,
    /// The terminal dock under the composer.
    TerminalDock,
}

impl Panel {
    pub const ALL: &'static [Panel] = &[Panel::Sidebar, Panel::RightPanel, Panel::TerminalDock];

    /// Shown in tooltips and the command palette.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sidebar => "Sidebar",
            Self::RightPanel => "Right Panel",
            Self::TerminalDock => "Terminal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    sidebar_open: bool,
    right_open: bool,
    dock_open: bool,
    sidebar_width: f32,
    right_width: f32,
    dock_height: f32,
}

impl Layout {
    pub fn from_settings(settings: &AppSettings) -> Self {
        Self {
            sidebar_open: settings.sidebar_open,
            right_open: settings.right_panel_open,
            dock_open: settings.terminal_dock_open,
            // Widths saved before the project rail existed describe a single
            // column and cannot fit the new two-pane navigator.
            sidebar_width: if settings.sidebar_width < 420.0 {
                520.0
            } else {
                settings.sidebar_width
            },
            right_width: settings.right_panel_width,
            dock_height: settings.terminal_dock_height,
        }
    }

    /// Restore global navigation plus the arrangement saved for `workspace`.
    pub fn for_workspace(settings: &AppSettings, workspace: Option<&WorkspaceId>) -> Self {
        let mut layout = Self::from_settings(settings);
        let Some(saved) =
            workspace.and_then(|workspace| settings.workspace_layouts.get(&workspace.0))
        else {
            return layout;
        };
        layout.right_open = saved.right_panel_open;
        layout.right_width = saved.right_panel_width;
        layout.dock_open = saved.terminal_dock_open;
        layout.dock_height = saved.terminal_dock_height;
        layout
    }

    /// Write the layout back over a settings value, leaving its other fields
    /// (the appearance, the locale) untouched.
    pub fn write_into(&self, settings: &mut AppSettings) {
        settings.sidebar_open = self.sidebar_open;
        settings.right_panel_open = self.right_open;
        settings.terminal_dock_open = self.dock_open;
        settings.sidebar_width = self.sidebar_width;
        settings.right_panel_width = self.right_width;
        settings.terminal_dock_height = self.dock_height;
    }

    /// Save workspace-owned panels while leaving global navigation untouched.
    pub fn write_workspace_into(&self, workspace: &WorkspaceId, settings: &mut AppSettings) {
        let active_surface = settings
            .workspace_layouts
            .get(&workspace.0)
            .and_then(|saved| saved.active_surface.clone());
        let terminal_split = settings
            .workspace_layouts
            .get(&workspace.0)
            .and_then(|saved| saved.terminal_split.clone());
        let terminal_active = settings
            .workspace_layouts
            .get(&workspace.0)
            .and_then(|saved| saved.terminal_active.clone());
        let surface_dock = settings
            .workspace_layouts
            .get(&workspace.0)
            .and_then(|saved| saved.surface_dock.clone());
        settings.workspace_layouts.insert(
            workspace.0.clone(),
            WorkspaceLayoutSettings {
                right_panel_open: self.right_open,
                right_panel_width: self.right_width,
                terminal_dock_open: self.dock_open,
                terminal_dock_height: self.dock_height,
                active_surface,
                terminal_split,
                terminal_active,
                surface_dock,
            },
        );
        settings.sidebar_open = self.sidebar_open;
        settings.sidebar_width = self.sidebar_width;
    }

    pub fn is_open(&self, panel: Panel) -> bool {
        match panel {
            Panel::Sidebar => self.sidebar_open,
            Panel::RightPanel => self.right_open,
            Panel::TerminalDock => self.dock_open,
        }
    }

    /// Returns true when the state actually changed, so callers can skip a
    /// redraw and a settings write that would be a no-op.
    pub fn toggle(&mut self, panel: Panel) -> bool {
        let open = !self.is_open(panel);
        self.set_open(panel, open);
        true
    }

    pub fn set_open(&mut self, panel: Panel, open: bool) {
        match panel {
            Panel::Sidebar => self.sidebar_open = open,
            Panel::RightPanel => self.right_open = open,
            Panel::TerminalDock => self.dock_open = open,
        }
    }

    /// Record a drag-resize. The size is kept even while the panel is closed,
    /// so reopening restores what the user had rather than a default.
    pub fn set_size(&mut self, panel: Panel, size: Pixels) {
        let value = f32::from(size);
        match panel {
            Panel::Sidebar => self.sidebar_width = value,
            Panel::RightPanel => self.right_width = value,
            Panel::TerminalDock => self.dock_height = value,
        }
    }

    /// What occupies each slot of the horizontal group, in render order.
    ///
    /// `None` is the centre column, which has no stored size. Closing a panel
    /// shifts every slot after it, so the resize callback must consult this
    /// rather than assume index 0 is the sidebar.
    pub fn columns(&self) -> Vec<Option<Panel>> {
        let mut slots = Vec::with_capacity(3);
        if self.sidebar_open {
            slots.push(Some(Panel::Sidebar));
        }
        slots.push(None);
        if self.right_open {
            slots.push(Some(Panel::RightPanel));
        }
        slots
    }

    /// The same, for the centre column's vertical split.
    pub fn rows(&self) -> Vec<Option<Panel>> {
        let mut slots = vec![None];
        if self.dock_open {
            slots.push(Some(Panel::TerminalDock));
        }
        slots
    }

    /// Record the sizes a drag produced, ignoring the centre column.
    ///
    /// Extra sizes are ignored rather than trusted: a mismatch means the
    /// toolkit's state and our slot map disagree, and guessing would write one
    /// panel's size over another's.
    pub fn record_sizes(&mut self, slots: &[Option<Panel>], sizes: &[Pixels]) {
        for (slot, size) in slots.iter().zip(sizes) {
            if let Some(panel) = slot {
                self.set_size(*panel, *size);
            }
        }
    }

    pub fn size(&self, panel: Panel) -> Pixels {
        px(match panel {
            Panel::Sidebar => self.sidebar_width,
            Panel::RightPanel => self.right_width,
            Panel::TerminalDock => self.dock_height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_core::settings::SurfaceArrangement;

    /// Settings with every panel open.
    ///
    /// The defaults start the right panel and the dock closed — they have
    /// nothing in them until M3 — but these tests are about how the
    /// arrangement behaves, not about what it starts as.
    fn all_open() -> AppSettings {
        AppSettings {
            sidebar_open: true,
            right_panel_open: true,
            terminal_dock_open: true,
            ..AppSettings::default()
        }
    }

    #[test]
    fn round_trips_through_settings() {
        let mut settings = all_open();
        let mut layout = Layout::from_settings(&settings);
        layout.toggle(Panel::RightPanel);
        layout.toggle(Panel::TerminalDock);
        layout.write_into(&mut settings);

        let restored = Layout::from_settings(&settings);
        assert_eq!(restored, layout);
        assert!(!restored.is_open(Panel::RightPanel));
        assert!(restored.is_open(Panel::Sidebar));
    }

    #[test]
    fn a_closed_panel_keeps_its_size() {
        let mut layout = Layout::from_settings(&AppSettings::default());
        layout.set_size(Panel::Sidebar, px(320.));
        layout.toggle(Panel::Sidebar);
        assert!(!layout.is_open(Panel::Sidebar));
        // Reopening must restore the user's width, not the default.
        layout.toggle(Panel::Sidebar);
        assert_eq!(layout.size(Panel::Sidebar), px(320.));
    }

    #[test]
    fn a_legacy_single_sidebar_width_expands_for_the_project_rail() {
        let settings = AppSettings {
            sidebar_width: 250.0,
            ..AppSettings::default()
        };
        let layout = Layout::from_settings(&settings);
        assert_eq!(layout.size(Panel::Sidebar), px(520.));
    }

    #[test]
    fn the_session_column_appears_only_after_a_project_is_selected() {
        assert_eq!(navigator_width(false, px(520.)), PROJECT_RAIL_WIDTH);
        assert_eq!(navigator_width(true, px(520.)), px(520.));
    }

    #[test]
    fn closing_every_panel_is_allowed() {
        // The centre column is not a panel, so there is no empty-window state
        // to guard against -- unlike VS Code, we have nothing to fall back to.
        let mut layout = Layout::from_settings(&AppSettings::default());
        for panel in Panel::ALL {
            layout.set_open(*panel, false);
        }
        assert!(Panel::ALL.iter().all(|panel| !layout.is_open(*panel)));
    }

    #[test]
    fn slots_track_which_panels_are_open() {
        let mut layout = Layout::from_settings(&all_open());
        assert_eq!(
            layout.columns(),
            vec![Some(Panel::Sidebar), None, Some(Panel::RightPanel)]
        );

        layout.set_open(Panel::Sidebar, false);
        // The centre column has moved to index 0.
        assert_eq!(layout.columns(), vec![None, Some(Panel::RightPanel)]);
    }

    #[test]
    fn a_resize_with_the_sidebar_closed_does_not_write_the_centre_width_into_it() {
        let mut layout = Layout::from_settings(&all_open());
        let original = layout.size(Panel::Sidebar);
        layout.set_open(Panel::Sidebar, false);

        // Index 0 is now the centre column, index 1 the right panel.
        layout.record_sizes(&layout.columns(), &[px(900.), px(500.)]);

        assert_eq!(layout.size(Panel::Sidebar), original);
        assert_eq!(layout.size(Panel::RightPanel), px(500.));
    }

    #[test]
    fn record_sizes_ignores_a_length_mismatch_rather_than_guessing() {
        let mut layout = Layout::from_settings(&all_open());
        let slots = layout.columns();
        // One size short: the right panel keeps whatever it had.
        let right = layout.size(Panel::RightPanel);
        layout.record_sizes(&slots, &[px(300.), px(900.)]);
        assert_eq!(layout.size(Panel::Sidebar), px(300.));
        assert_eq!(layout.size(Panel::RightPanel), right);
    }

    #[test]
    fn rows_carry_the_dock_only_when_it_is_open() {
        let mut layout = Layout::from_settings(&all_open());
        assert_eq!(layout.rows(), vec![None, Some(Panel::TerminalDock)]);
        layout.set_open(Panel::TerminalDock, false);
        assert_eq!(layout.rows(), vec![None]);
    }

    #[test]
    fn the_arrangement_survives_a_restart() {
        // The one path that cannot be exercised by clicking: toggle, write to
        // disk, and read it back the way the next launch will.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.json");

        let mut settings = all_open();
        let mut layout = Layout::from_settings(&settings);
        layout.toggle(Panel::RightPanel);
        layout.set_size(Panel::Sidebar, px(512.));
        layout.set_size(Panel::TerminalDock, px(180.));
        layout.write_into(&mut settings);
        ginka_core::settings::save(&path, &settings).unwrap();

        let reloaded: AppSettings = ginka_core::settings::load(&path);
        let restored = Layout::from_settings(&reloaded);
        assert_eq!(restored, layout);
        assert!(!restored.is_open(Panel::RightPanel));
        assert_eq!(restored.size(Panel::Sidebar), px(512.));
        assert_eq!(restored.size(Panel::TerminalDock), px(180.));
    }

    #[test]
    fn workspace_arrangements_override_surfaces_but_share_navigation() {
        let mut settings = all_open();
        settings.workspace_layouts.insert(
            "comet/main".into(),
            WorkspaceLayoutSettings {
                right_panel_open: false,
                right_panel_width: 560.0,
                terminal_dock_open: true,
                terminal_dock_height: 180.0,
                active_surface: Some("files".into()),
                terminal_split: None,
                terminal_active: None,
                surface_dock: None,
            },
        );
        let workspace = WorkspaceId("comet/main".into());
        let layout = Layout::for_workspace(&settings, Some(&workspace));

        assert!(layout.is_open(Panel::Sidebar));
        assert!(!layout.is_open(Panel::RightPanel));
        assert!(layout.is_open(Panel::TerminalDock));
        assert_eq!(layout.size(Panel::RightPanel), px(560.0));
        assert_eq!(layout.size(Panel::TerminalDock), px(180.0));
    }

    #[test]
    fn saving_one_workspace_preserves_its_surface_and_other_arrangements() {
        let mut settings = all_open();
        settings.workspace_layouts.insert(
            "comet/main".into(),
            WorkspaceLayoutSettings {
                active_surface: Some("files".into()),
                terminal_split: Some(["left".into(), "right".into()]),
                terminal_active: Some("right".into()),
                surface_dock: Some(SurfaceArrangement::Tabs {
                    surfaces: vec!["files".into(), "git".into()],
                    active: 0,
                }),
                ..WorkspaceLayoutSettings::default()
            },
        );
        let other = WorkspaceLayoutSettings {
            right_panel_open: true,
            right_panel_width: 640.0,
            terminal_dock_open: true,
            terminal_dock_height: 300.0,
            active_surface: Some("git".into()),
            terminal_split: None,
            terminal_active: None,
            surface_dock: None,
        };
        settings
            .workspace_layouts
            .insert("comet/review".into(), other.clone());

        let workspace = WorkspaceId("comet/main".into());
        let mut layout = Layout::for_workspace(&settings, Some(&workspace));
        layout.set_open(Panel::RightPanel, true);
        layout.set_size(Panel::RightPanel, px(510.0));
        layout.write_workspace_into(&workspace, &mut settings);

        let saved = settings.workspace_layouts.get(&workspace.0).unwrap();
        assert_eq!(saved.active_surface.as_deref(), Some("files"));
        assert_eq!(saved.terminal_split, Some(["left".into(), "right".into()]));
        assert_eq!(saved.terminal_active.as_deref(), Some("right"));
        assert_eq!(
            saved.surface_dock,
            Some(SurfaceArrangement::Tabs {
                surfaces: vec!["files".into(), "git".into()],
                active: 0,
            })
        );
        assert_eq!(saved.right_panel_width, 510.0);
        assert_eq!(settings.workspace_layouts.get("comet/review"), Some(&other));
    }

    #[test]
    fn every_panel_is_labelled() {
        for panel in Panel::ALL {
            assert!(!panel.label().is_empty());
        }
    }
}
