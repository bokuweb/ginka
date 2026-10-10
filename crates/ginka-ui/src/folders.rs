//! Project-local destinations for the sidebar's folder picker.

use ginka_protocol::{ProjectName, WorkspaceId};

/// A picker action kept separate from its translated display label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Remove the workspace's folder label.
    Ungrouped,
    /// Ask for a new label in the name field.
    New,
    /// Assign an existing, exact case-sensitive label.
    Existing(String),
}

/// Build choices from all registered rows, before search, pin or archive filtering.
/// Archived-only folders remain valid destinations within their own project.
pub fn destinations<'a>(
    project: &ProjectName,
    rows: impl IntoIterator<Item = (&'a WorkspaceId, Option<&'a str>)>,
) -> Vec<Destination> {
    let names = rows
        .into_iter()
        .filter_map(|(workspace, folder)| {
            let (owner, _) = workspace.parts()?;
            (owner == *project).then_some(folder).flatten()
        })
        .collect::<std::collections::BTreeSet<_>>();
    [Destination::Ungrouped, Destination::New]
        .into_iter()
        .chain(
            names
                .into_iter()
                .map(|name| Destination::Existing(name.to_owned())),
        )
        .collect()
}

/// Ephemeral batch selection, separate from the conversation being read.
/// The anchor follows visible order; refreshes discard hidden targets.
#[derive(Debug, Default)]
pub struct Selection {
    selected: std::collections::BTreeSet<WorkspaceId>,
    anchor: Option<WorkspaceId>,
}

impl Selection {
    /// Remember a normal row click as the next range's starting point.
    pub fn anchor(&mut self, workspace: &WorkspaceId) {
        self.anchor = Some(workspace.clone());
    }

    /// Toggle one row without opening its conversation.
    pub fn toggle(&mut self, workspace: &WorkspaceId) {
        if !self.selected.remove(workspace) {
            self.selected.insert(workspace.clone());
        }
        self.anchor(workspace);
    }

    /// Replace the selection with the inclusive displayed range. With no
    /// visible anchor, select only the clicked row.
    pub fn range(&mut self, workspace: &WorkspaceId, order: &[WorkspaceId]) {
        let Some(end) = order.iter().position(|id| id == workspace) else {
            return;
        };
        let start = self
            .anchor
            .as_ref()
            .and_then(|anchor| order.iter().position(|id| id == anchor))
            .unwrap_or(end);
        self.selected = order[start.min(end)..=start.max(end)]
            .iter()
            .cloned()
            .collect();
        if self.anchor.is_none() {
            self.anchor(workspace);
        }
    }

    /// Forget targets and anchors no longer visible after filtering or refresh.
    pub fn retain(&mut self, order: &[WorkspaceId]) {
        self.selected.retain(|id| order.contains(id));
        if self.anchor.as_ref().is_some_and(|id| !order.contains(id)) {
            self.anchor = None;
        }
    }

    /// Whether this row is checked for the batch operation.
    pub fn contains(&self, workspace: &WorkspaceId) -> bool {
        self.selected.contains(workspace)
    }

    /// Snapshot the checked ids in displayed order, never including hidden ids.
    pub fn targets(&self, order: &[WorkspaceId]) -> Vec<WorkspaceId> {
        order
            .iter()
            .filter(|id| self.selected.contains(*id))
            .cloned()
            .collect()
    }
}

/// A destination may be submitted only for 1–256 visible ids from one project.
/// This also cancels frozen picker targets after a filter or project change.
pub fn valid_targets(targets: &[WorkspaceId], order: &[WorkspaceId]) -> bool {
    let Some((project, _)) = targets.first().and_then(WorkspaceId::parts) else {
        return false;
    };
    (1..=256).contains(&targets.len())
        && targets
            .iter()
            .all(|id| order.contains(id) && id.parts().is_some_and(|(owner, _)| owner == project))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(names: &[&str]) -> Vec<WorkspaceId> {
        names
            .iter()
            .map(|name| WorkspaceId(format!("comet/{name}")))
            .collect()
    }

    #[test]
    fn frozen_targets_require_visible_project_local_bounded_membership() {
        let order = ids(&["a", "b", "c"]);
        let frozen = order[..2].to_vec();
        assert!(valid_targets(&frozen, &order));
        assert!(!valid_targets(&frozen, &order[1..]));
        assert!(!valid_targets(&[], &order));
        assert!(!valid_targets(&vec![order[0].clone(); 257], &order));
        assert!(valid_targets(&vec![order[0].clone(); 256], &order));
        let mixed = [order[0].clone(), WorkspaceId("other/a".into())];
        assert!(!valid_targets(&mixed, &mixed));
        let malformed = [WorkspaceId("broken".into())];
        assert!(!valid_targets(&malformed, &malformed));
    }

    #[test]
    fn selection_ranges_follow_visible_order_and_keep_the_original_anchor() {
        let order = ids(&["c", "a", "b", "d"]);
        let mut selection = Selection::default();
        selection.anchor(&order[0]);
        selection.range(&order[2], &order);
        assert_eq!(selection.targets(&order), ids(&["c", "a", "b"]));
        selection.range(&order[1], &order);
        assert_eq!(selection.targets(&order), ids(&["c", "a"]));
        selection.toggle(&order[3]);
        assert_eq!(selection.targets(&order), ids(&["c", "a", "d"]));
        selection.toggle(&order[1]);
        assert_eq!(selection.targets(&order), ids(&["c", "d"]));
    }

    #[test]
    fn selection_drops_hidden_removed_and_foreign_rows_without_resurrecting_them() {
        let order = ids(&["a", "b", "c"]);
        let mut selection = Selection::default();
        selection.range(&order[1], &order);
        assert_eq!(selection.targets(&order), ids(&["b"]));
        selection.anchor(&order[0]);
        selection.range(&order[2], &order);
        selection.retain(&ids(&["b", "c"]));
        assert_eq!(selection.targets(&order), ids(&["b", "c"]));
        selection.range(&order[1], &order);
        assert_eq!(selection.targets(&order), ids(&["b"]));
        selection.retain(&[WorkspaceId("other/a".into())]);
        assert!(selection.targets(&order).is_empty());
    }

    #[test]
    fn choices_deduplicate_and_sort_exact_names_with_no_cross_project_leak() {
        let rows = [
            (WorkspaceId("comet/pinned".into()), Some("Review")),
            (WorkspaceId("comet/archived".into()), Some("Archive only")),
            (WorkspaceId("comet/duplicate".into()), Some("Review")),
            (WorkspaceId("other/foreign".into()), Some("Foreign")),
            (WorkspaceId("comet/ungrouped".into()), None),
            (WorkspaceId("comet/lower".into()), Some("review")),
        ];
        assert_eq!(
            destinations(
                &ProjectName("comet".into()),
                rows.iter().map(|(id, name)| (id, *name))
            ),
            vec![
                Destination::Ungrouped,
                Destination::New,
                Destination::Existing("Archive only".into()),
                Destination::Existing("Review".into()),
                Destination::Existing("review".into()),
            ]
        );
    }

    #[test]
    fn action_labels_cannot_shadow_real_folder_names() {
        let id = WorkspaceId("comet/a".into());
        assert_eq!(
            destinations(&ProjectName("comet".into()), [(&id, Some("New folder…"))]),
            vec![
                Destination::Ungrouped,
                Destination::New,
                Destination::Existing("New folder…".into())
            ]
        );
        assert_eq!(
            destinations(&ProjectName("empty".into()), [(&id, None)]),
            vec![Destination::Ungrouped, Destination::New]
        );
    }
}
