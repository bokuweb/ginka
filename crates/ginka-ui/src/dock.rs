//! How the right panel's surfaces are arranged: which are open, which share a
//! tab group, and how the groups split the panel (`docs/ui.md` §3.4).
//!
//! The dock area owns the arrangement while the user drags tabs about; this is
//! the arrangement at rest — what is saved per workspace, what is rebuilt when
//! the workspace comes back, and what a command changes when it opens a
//! surface. It is kept apart from the dock's own saved state because that
//! state names panels, not surfaces, and carries whatever a newer or older
//! build left in it.

use crate::surface::Surface;
use ginka_core::settings::SurfaceArrangement;
use gpui_component::dock::{PanelInfo, PanelState};

/// One node of the arrangement.
#[derive(Debug, Clone, PartialEq)]
pub enum DockNode {
    /// Groups side by side, or one above another.
    Split {
        /// Whether the children are stacked top to bottom.
        vertical: bool,
        /// The groups or further splits, in order. Never fewer than two.
        children: Vec<DockNode>,
        /// Each child's size in logical pixels, where one was dragged to.
        sizes: Vec<Option<f32>>,
    },
    /// Surfaces sharing one area as tabs. Never empty.
    Tabs {
        /// The surfaces in tab order.
        surfaces: Vec<Surface>,
        /// The index of the tab in front, always in range.
        active: usize,
    },
}

/// The right panel's arrangement of surfaces.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurfaceDock {
    root: Option<DockNode>,
}

impl SurfaceDock {
    /// A panel with no surface open.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Restore a saved arrangement, or — for a workspace saved before
    /// surfaces could be arranged — the one surface that was selected.
    ///
    /// A surface this build does not know, a surface saved twice, an empty
    /// group and a split left with one child are all mended away rather than
    /// refused: a layout is a convenience, and losing all of it over one
    /// stale tab would be worse than losing the tab.
    pub fn restore(saved: Option<&SurfaceArrangement>, fallback: Option<Surface>) -> Self {
        let Some(saved) = saved else {
            return Self {
                root: fallback.map(|surface| DockNode::Tabs {
                    surfaces: vec![surface],
                    active: 0,
                }),
            };
        };
        let mut seen = Vec::new();
        Self {
            root: from_arrangement(saved, &mut seen),
        }
    }

    /// The arrangement as it is saved.
    pub fn save(&self) -> Option<SurfaceArrangement> {
        self.root.as_ref().map(to_arrangement)
    }

    /// Read back what the dock area holds after the user rearranged it.
    ///
    /// Panels that are not surfaces are left out, and the dock's habit of
    /// wrapping a lone group in a split is undone.
    pub fn from_panel_state(state: &PanelState) -> Self {
        let mut seen = Vec::new();
        Self {
            root: from_panel_state(state, &mut seen),
        }
    }

    /// The arrangement's root, when anything is open.
    pub fn root(&self) -> Option<&DockNode> {
        self.root.as_ref()
    }

    /// Whether no surface is open.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Bring a surface to the front: its own tab when it is open, a new tab
    /// in the first group when it is not. Says whether anything changed.
    pub fn open(&mut self, surface: Surface) -> bool {
        let Some(root) = self.root.as_mut() else {
            self.root = Some(DockNode::Tabs {
                surfaces: vec![surface],
                active: 0,
            });
            return true;
        };
        if let Some((surfaces, active)) = group_holding(root, surface) {
            let index = surfaces
                .iter()
                .position(|open| *open == surface)
                .unwrap_or(*active);
            let changed = *active != index;
            *active = index;
            return changed;
        }
        let (surfaces, active) = first_group(root);
        surfaces.push(surface);
        *active = surfaces.len() - 1;
        true
    }

    /// Close a surface's tab. Says whether it was open.
    pub fn close(&mut self, surface: Surface) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let Some((surfaces, active)) = group_holding(root, surface) else {
            return false;
        };
        if let Some(index) = surfaces.iter().position(|open| *open == surface) {
            surfaces.remove(index);
            if index < *active || *active >= surfaces.len() {
                *active = active.saturating_sub(1);
            }
        }
        let mut seen = Vec::new();
        self.root = self.root.take().and_then(|root| mend(root, &mut seen));
        true
    }

    /// Whether a surface has a tab anywhere in the panel.
    pub fn contains(&self, surface: Surface) -> bool {
        let mut all = Vec::new();
        if let Some(root) = &self.root {
            collect(root, &mut all, false);
        }
        all.contains(&surface)
    }

    /// The surfaces on screen: the front tab of every group, in order.
    pub fn visible(&self) -> Vec<Surface> {
        let mut visible = Vec::new();
        if let Some(root) = &self.root {
            collect(root, &mut visible, true);
        }
        visible
    }
}

/// Every surface under `node`, or only the front tab of each group.
fn collect(node: &DockNode, into: &mut Vec<Surface>, front_only: bool) {
    match node {
        DockNode::Split { children, .. } => {
            for child in children {
                collect(child, into, front_only);
            }
        }
        DockNode::Tabs { surfaces, active } if front_only => {
            into.extend(surfaces.get(*active).copied());
        }
        DockNode::Tabs { surfaces, .. } => into.extend(surfaces.iter().copied()),
    }
}

/// The group a surface has its tab in.
fn group_holding(node: &mut DockNode, surface: Surface) -> Option<(&mut Vec<Surface>, &mut usize)> {
    match node {
        DockNode::Split { children, .. } => children
            .iter_mut()
            .find_map(|child| group_holding(child, surface)),
        DockNode::Tabs { surfaces, active } => {
            if surfaces.contains(&surface) {
                Some((surfaces, active))
            } else {
                None
            }
        }
    }
}

/// The first group in reading order, which is where a new tab goes.
fn first_group(node: &mut DockNode) -> (&mut Vec<Surface>, &mut usize) {
    match node {
        // A split is never empty once mended.
        DockNode::Split { children, .. } => first_group(&mut children[0]),
        DockNode::Tabs { surfaces, active } => (surfaces, active),
    }
}

/// A group of the surfaces that are still to be placed, keeping whichever
/// was in front in front.
fn group(
    candidates: Vec<Option<Surface>>,
    active: usize,
    seen: &mut Vec<Surface>,
) -> Option<DockNode> {
    let front = candidates.get(active).copied().flatten();
    let mut surfaces = Vec::new();
    for surface in candidates.into_iter().flatten() {
        if !seen.contains(&surface) {
            seen.push(surface);
            surfaces.push(surface);
        }
    }
    if surfaces.is_empty() {
        return None;
    }
    let active = front
        .and_then(|front| surfaces.iter().position(|surface| *surface == front))
        .unwrap_or(0);
    Some(DockNode::Tabs { surfaces, active })
}

/// A split of whichever children survived, or the one child when only one
/// did.
fn split(vertical: bool, children: Vec<(Option<DockNode>, Option<f32>)>) -> Option<DockNode> {
    let (children, sizes): (Vec<_>, Vec<_>) = children
        .into_iter()
        .filter_map(|(child, size)| child.map(|child| (child, size)))
        .unzip();
    match children.len() {
        0 => None,
        1 => children.into_iter().next(),
        _ => Some(DockNode::Split {
            vertical,
            children,
            sizes,
        }),
    }
}

fn mend(node: DockNode, seen: &mut Vec<Surface>) -> Option<DockNode> {
    match node {
        DockNode::Split {
            vertical,
            children,
            sizes,
        } => {
            let children = children
                .into_iter()
                .enumerate()
                .map(|(index, child)| (mend(child, seen), sizes.get(index).copied().flatten()))
                .collect();
            split(vertical, children)
        }
        DockNode::Tabs { surfaces, active } => {
            group(surfaces.into_iter().map(Some).collect(), active, seen)
        }
    }
}

fn from_arrangement(saved: &SurfaceArrangement, seen: &mut Vec<Surface>) -> Option<DockNode> {
    match saved {
        SurfaceArrangement::Split {
            vertical,
            children,
            sizes,
        } => {
            let children = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    (
                        from_arrangement(child, seen),
                        sizes.get(index).copied().flatten(),
                    )
                })
                .collect();
            split(*vertical, children)
        }
        SurfaceArrangement::Tabs { surfaces, active } => group(
            surfaces.iter().map(|key| Surface::from_key(key)).collect(),
            *active,
            seen,
        ),
    }
}

fn to_arrangement(node: &DockNode) -> SurfaceArrangement {
    match node {
        DockNode::Split {
            vertical,
            children,
            sizes,
        } => SurfaceArrangement::Split {
            vertical: *vertical,
            children: children.iter().map(to_arrangement).collect(),
            sizes: sizes.clone(),
        },
        DockNode::Tabs { surfaces, active } => SurfaceArrangement::Tabs {
            surfaces: surfaces
                .iter()
                .map(|surface| surface.key().to_string())
                .collect(),
            active: *active,
        },
    }
}

fn from_panel_state(state: &PanelState, seen: &mut Vec<Surface>) -> Option<DockNode> {
    match &state.info {
        PanelInfo::Stack { sizes, .. } => {
            let vertical = state.info.axis() == Some(gpui::Axis::Vertical);
            let children = state
                .children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    // Zero is how the dock writes a size nobody dragged to.
                    let size = sizes
                        .get(index)
                        .map(|size| size.as_f32())
                        .filter(|size| *size > 0.0);
                    (from_panel_state(child, seen), size)
                })
                .collect();
            split(vertical, children)
        }
        PanelInfo::Tabs { active_index } => group(
            state
                .children
                .iter()
                .map(|child| Surface::from_panel_name(&child.panel_name))
                .collect(),
            *active_index,
            seen,
        ),
        PanelInfo::Tiles { .. } => group(
            state
                .children
                .iter()
                .map(|child| Surface::from_panel_name(&child.panel_name))
                .collect(),
            0,
            seen,
        ),
        PanelInfo::Panel(_) => group(vec![Surface::from_panel_name(&state.panel_name)], 0, seen),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    fn tabs(surfaces: &[Surface], active: usize) -> DockNode {
        DockNode::Tabs {
            surfaces: surfaces.to_vec(),
            active,
        }
    }

    #[test]
    fn opening_a_surface_adds_a_tab_and_opening_it_again_brings_it_forward() {
        let mut dock = SurfaceDock::empty();
        assert!(dock.is_empty());
        assert!(dock.open(Surface::Git));
        assert!(dock.open(Surface::Files));
        assert_eq!(dock.root(), Some(&tabs(&[Surface::Git, Surface::Files], 1)));
        assert_eq!(dock.visible(), vec![Surface::Files]);

        assert!(dock.open(Surface::Git));
        assert_eq!(dock.visible(), vec![Surface::Git]);
        assert!(!dock.open(Surface::Git), "already in front");
        assert!(dock.contains(Surface::Files));
        assert!(!dock.contains(Surface::Browser));
    }

    #[test]
    fn closing_the_last_tab_of_a_group_folds_its_split_away() {
        let saved = SurfaceArrangement::Split {
            vertical: true,
            children: vec![
                SurfaceArrangement::Tabs {
                    surfaces: vec!["git".into(), "files".into()],
                    active: 0,
                },
                SurfaceArrangement::Tabs {
                    surfaces: vec!["browser".into()],
                    active: 0,
                },
            ],
            sizes: vec![Some(300.0), None],
        };
        let mut dock = SurfaceDock::restore(Some(&saved), None);
        assert_eq!(dock.visible(), vec![Surface::Git, Surface::Browser]);

        assert!(dock.close(Surface::Browser));
        assert_eq!(dock.root(), Some(&tabs(&[Surface::Git, Surface::Files], 0)));
        assert!(!dock.close(Surface::Browser));

        assert!(dock.close(Surface::Git));
        assert_eq!(dock.visible(), vec![Surface::Files]);
        assert!(dock.close(Surface::Files));
        assert!(dock.is_empty());
        assert_eq!(dock.save(), None);
    }

    #[test]
    fn an_arrangement_survives_a_save_and_restore() {
        let mut dock = SurfaceDock::empty();
        dock.open(Surface::Git);
        dock.open(Surface::Files);
        let saved = dock.save().expect("something is open");
        assert_eq!(SurfaceDock::restore(Some(&saved), None), dock);

        let split = SurfaceArrangement::Split {
            vertical: false,
            children: vec![
                SurfaceArrangement::Tabs {
                    surfaces: vec!["reports".into()],
                    active: 0,
                },
                SurfaceArrangement::Tabs {
                    surfaces: vec!["skills".into(), "files".into()],
                    active: 1,
                },
            ],
            sizes: vec![Some(200.0), None],
        };
        let restored = SurfaceDock::restore(Some(&split), None);
        assert_eq!(restored.save(), Some(split));
    }

    #[test]
    fn a_restore_drops_what_this_build_cannot_show_and_mends_the_rest() {
        let saved = SurfaceArrangement::Split {
            vertical: true,
            children: vec![
                // A surface from a newer build, and a duplicate.
                SurfaceArrangement::Tabs {
                    surfaces: vec!["hologram".into(), "git".into(), "git".into()],
                    active: 9,
                },
                // Nothing this build knows: the group goes, and with it the
                // split, which is left with one child.
                SurfaceArrangement::Tabs {
                    surfaces: vec!["hologram".into()],
                    active: 0,
                },
                SurfaceArrangement::Split {
                    vertical: false,
                    children: vec![],
                    sizes: vec![],
                },
            ],
            sizes: vec![Some(10.0)],
        };
        let dock = SurfaceDock::restore(Some(&saved), Some(Surface::Files));
        assert_eq!(dock.root(), Some(&tabs(&[Surface::Git], 0)));
    }

    #[test]
    fn a_workspace_saved_before_arrangements_opens_its_selected_surface() {
        let dock = SurfaceDock::restore(None, Some(Surface::Reports));
        assert_eq!(dock.root(), Some(&tabs(&[Surface::Reports], 0)));
        assert!(SurfaceDock::restore(None, None).is_empty());
    }

    #[test]
    fn the_dock_areas_own_state_reads_back_as_surfaces() {
        let tab_group = |names: &[&str], active: usize| PanelState {
            panel_name: "TabPanel".into(),
            children: names.iter().map(|name| PanelState::new(*name)).collect(),
            info: PanelInfo::tabs(active),
        };
        let state = PanelState {
            panel_name: "StackPanel".into(),
            children: vec![
                tab_group(&[Surface::Git.panel_name(), "SomethingElse"], 0),
                tab_group(&[Surface::Browser.panel_name()], 0),
                tab_group(&[], 0),
            ],
            info: PanelInfo::stack(vec![px(240.0), px(0.0), px(0.0)], gpui::Axis::Vertical),
        };
        let dock = SurfaceDock::from_panel_state(&state);
        assert_eq!(
            dock.root(),
            Some(&DockNode::Split {
                vertical: true,
                children: vec![tabs(&[Surface::Git], 0), tabs(&[Surface::Browser], 0)],
                // A zero size is the dock's way of saying "not dragged".
                sizes: vec![Some(240.0), None],
            })
        );

        // The dock wraps even a single group in a split; that reads back as
        // the group alone.
        let single = PanelState {
            panel_name: "StackPanel".into(),
            children: vec![tab_group(&[Surface::Files.panel_name()], 0)],
            info: PanelInfo::stack(vec![px(0.0)], gpui::Axis::Horizontal),
        };
        assert_eq!(
            SurfaceDock::from_panel_state(&single).root(),
            Some(&tabs(&[Surface::Files], 0))
        );
    }

    #[test]
    fn every_surface_has_its_own_panel_name() {
        for surface in Surface::ALL {
            assert_eq!(
                Surface::from_panel_name(surface.panel_name()),
                Some(*surface)
            );
        }
        assert_eq!(Surface::from_panel_name("TabPanel"), None);
    }
}
