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

#[cfg(test)]
mod tests {
    use super::*;

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
