//! The session list as a flat run of items, for a virtualized list.
//!
//! The sidebar draws project headings, the rows under them, and the archived
//! section after them. A virtualized list wants one item per thing it draws
//! and a stable way to tell what changed between two frames, so the shape is
//! decided here, with tests, and the view only draws an item.

use crate::workspace::ProjectGroup;
use ginka_protocol::ProjectName;

/// One item of the session list. Rows are addressed by their index in the
/// sidebar's flat list of rows, which is what selection is addressed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A project's heading, where more than one project is listed.
    Project(ProjectName),
    /// A project-local folder heading; no directory is created or moved.
    Folder(ProjectName, String),
    /// A live session row.
    Row(usize),
    /// The archived section's heading, which opens and closes it.
    ArchivedHeading,
    /// An archived row, while the section is open.
    Archived(usize),
    /// The note under an open archived section.
    ArchivedFoot,
}

/// Lay the list out: each group's heading (when `headings`) and rows, then
/// the archived section when there is anything archived.
pub fn entries(
    groups: &[ProjectGroup],
    headings: bool,
    archived: &[usize],
    archived_open: bool,
) -> Vec<Entry> {
    entries_with_folders(
        groups,
        headings,
        archived,
        archived_open,
        &Default::default(),
        false,
    )
}

/// Lay out project-local folder headings even while their members are hidden.
/// Pins remain visible; searching temporarily reveals matching members without
/// modifying the saved preference. Callers filter search matches before layout.
pub fn entries_with_folders(
    groups: &[ProjectGroup],
    headings: bool,
    archived: &[usize],
    archived_open: bool,
    collapsed: &ginka_core::settings::CollapsedWorkspaceFolders,
    searching: bool,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    for group in groups {
        if headings {
            entries.push(Entry::Project(group.name.clone()));
        }
        let mut rows = group.rows.iter().collect::<Vec<_>>();
        rows.sort_by(|(_, left), (_, right)| left.compare_list(right));
        let mut folder = None;
        for (index, row) in rows {
            if !row.pinned && row.folder.as_ref() != folder {
                folder = row.folder.as_ref();
                if let Some(name) = folder {
                    entries.push(Entry::Folder(group.name.clone(), name.clone()));
                }
            }
            if !searching
                && !row.pinned
                && row
                    .folder
                    .as_ref()
                    .is_some_and(|name| collapsed.is_collapsed(&group.name, name))
            {
                continue;
            }
            entries.push(Entry::Row(*index));
        }
    }
    if !archived.is_empty() {
        entries.push(Entry::ArchivedHeading);
        if archived_open {
            entries.extend(archived.iter().map(|index| Entry::Archived(*index)));
            entries.push(Entry::ArchivedFoot);
        }
    }
    entries
}

/// Row indices in displayed order, including archives only while visible.
/// Headings and footnotes cannot become batch-selection targets.
pub fn row_indices(entries: &[Entry]) -> Vec<usize> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Row(index) | Entry::Archived(index) => Some(*index),
            _ => None,
        })
        .collect()
}

/// The first item that differs between two layouts — where a list must
/// measure again from — or `None` when they are the same.
pub fn first_difference(before: &[Entry], after: &[Entry]) -> Option<usize> {
    before
        .iter()
        .zip(after)
        .position(|(was, is)| was != is)
        .or_else(|| (before.len() != after.len()).then(|| before.len().min(after.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::SessionRow;

    fn group(name: &str, rows: &[usize]) -> ProjectGroup {
        let row = SessionRow::samples().remove(0);
        ProjectGroup {
            name: ProjectName(name.into()),
            project: name.to_string().into(),
            rows: rows.iter().map(|index| (*index, row.clone())).collect(),
        }
    }

    #[test]
    fn folders_group_rows_after_pins_and_keyboard_order_matches() {
        let mut group = group("comet", &[0, 1, 2, 3]);
        for (index, row) in &mut group.rows {
            row.workspace = ginka_protocol::WorkspaceId(format!("comet/work-{index}"));
            row.origin = "comet".into();
            row.pinned = false;
        }
        group.rows[0].1.folder = Some("Review".into());
        group.rows[1].1.folder = Some("Build".into());
        group.rows[2].1.folder = Some("Review".into());
        group.rows[2].1.pinned = true;
        let layout = entries(&[group.clone()], false, &[], false);
        assert_eq!(
            layout,
            [
                Entry::Row(2),
                Entry::Row(3),
                Entry::Folder(ProjectName("comet".into()), "Build".into()),
                Entry::Row(1),
                Entry::Folder(ProjectName("comet".into()), "Review".into()),
                Entry::Row(0)
            ]
        );
        let rows = group
            .rows
            .iter()
            .map(|(_, row)| row.clone())
            .collect::<Vec<_>>();
        let keyboard = crate::workspace::visible_sessions(&rows, &group.name, "");
        let displayed = layout
            .iter()
            .filter_map(|entry| match entry {
                Entry::Row(index) => Some(rows[*index].workspace.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(keyboard, displayed);
        assert!(crate::workspace::session_matches(&rows[0], "review"));
        assert!(!crate::workspace::session_matches(&rows[3], "review"));
    }

    #[test]
    fn folders_are_project_local_and_attention_sort_is_stable_inside_them() {
        let mut first = group("comet", &[0, 1, 2]);
        let mut second = group("aurora", &[3]);
        for (_, row) in first.rows.iter_mut().chain(second.rows.iter_mut()) {
            row.pinned = false;
            row.folder = Some("Review".into());
            row.state = crate::workspace::AgentState::Idle;
        }
        first.rows[1].1.state = crate::workspace::AgentState::Working;
        assert_eq!(
            entries(&[first, second], true, &[], false),
            [
                Entry::Project(ProjectName("comet".into())),
                Entry::Folder(ProjectName("comet".into()), "Review".into()),
                Entry::Row(1),
                Entry::Row(0),
                Entry::Row(2),
                Entry::Project(ProjectName("aurora".into())),
                Entry::Folder(ProjectName("aurora".into()), "Review".into()),
                Entry::Row(3),
            ]
        );
    }

    #[test]
    fn collapsed_folders_hide_only_unpinned_members_and_keep_keyboard_order() {
        let mut comet = group("comet", &[0, 1, 2]);
        let mut aurora = group("aurora", &[3]);
        for (index, row) in comet.rows.iter_mut().chain(aurora.rows.iter_mut()) {
            row.workspace = ginka_protocol::WorkspaceId(format!(
                "{}/work-{index}",
                if *index == 3 { "aurora" } else { "comet" }
            ));
            row.origin = if *index == 3 {
                "aurora".into()
            } else {
                "comet".into()
            };
            row.pinned = false;
            row.folder = Some("Review".into());
        }
        comet.rows[1].1.pinned = true;
        comet.rows[2].1.folder = None;
        let mut collapsed = ginka_core::settings::CollapsedWorkspaceFolders::default();
        collapsed.set_collapsed(&comet.name, "Review", true);
        let groups = [comet.clone(), aurora];
        let layout = entries_with_folders(&groups, true, &[4], true, &collapsed, false);
        assert_eq!(row_indices(&layout), [1, 2, 3, 4]);
        assert!(layout.contains(&Entry::Folder(comet.name.clone(), "Review".into())));
        let rows = comet
            .rows
            .iter()
            .map(|(_, row)| row.clone())
            .collect::<Vec<_>>();
        let keyboard =
            crate::workspace::visible_sessions_with_folders(&rows, &comet.name, "", &collapsed);
        assert_eq!(
            keyboard,
            [rows[1].workspace.clone(), rows[2].workspace.clone()]
        );
        assert_eq!(
            crate::workspace::adjacent_session(&keyboard, Some(&rows[1].workspace), true),
            Some(rows[2].workspace.clone())
        );
        assert_eq!(
            crate::workspace::adjacent_session(&keyboard, Some(&rows[2].workspace), true),
            Some(rows[1].workspace.clone())
        );

        let searching = entries_with_folders(&groups, true, &[], false, &collapsed, true);
        assert_eq!(row_indices(&searching), [1, 2, 0, 3]);
        assert_eq!(
            crate::workspace::visible_sessions_with_folders(
                &rows,
                &comet.name,
                "Review",
                &collapsed
            ),
            [rows[1].workspace.clone(), rows[0].workspace.clone()]
        );
        assert!(
            collapsed.is_collapsed(&comet.name, "Review"),
            "search must not change the saved preference"
        );

        let mut selected = crate::folders::Selection::default();
        selected.toggle(&rows[0].workspace);
        selected.toggle(&rows[1].workspace);
        selected.retain(&keyboard);
        assert_eq!(selected.targets(&keyboard), [rows[1].workspace.clone()]);
        collapsed.set_collapsed(&comet.name, "Review", false);
        let reopened =
            crate::workspace::visible_sessions_with_folders(&rows, &comet.name, "", &collapsed);
        selected.retain(&reopened);
        assert_eq!(
            selected.targets(&reopened),
            [rows[1].workspace.clone()],
            "hidden selections must not return"
        );
        assert!(!crate::folders::valid_targets(
            &[rows[0].workspace.clone()],
            &keyboard
        ));
    }

    #[test]
    fn headings_rows_then_the_archived_section() {
        let groups = [group("comet", &[0, 1]), group("aurora", &[2])];
        assert_eq!(
            entries(&groups, true, &[3, 4], true),
            [
                Entry::Project(ProjectName("comet".into())),
                Entry::Row(0),
                Entry::Row(1),
                Entry::Project(ProjectName("aurora".into())),
                Entry::Row(2),
                Entry::ArchivedHeading,
                Entry::Archived(3),
                Entry::Archived(4),
                Entry::ArchivedFoot,
            ]
        );
    }

    #[test]
    fn one_project_needs_no_heading_and_a_closed_archive_is_one_line() {
        let groups = [group("comet", &[0])];
        assert_eq!(
            entries(&groups, false, &[1], false),
            [Entry::Row(0), Entry::ArchivedHeading]
        );
        assert_eq!(entries(&groups, false, &[], true), [Entry::Row(0)]);
        assert!(entries(&[], true, &[], true).is_empty());
    }

    #[test]
    fn batch_selection_includes_visible_archives_and_drops_them_on_collapse() {
        let groups = [group("comet", &[0, 1])];
        let ids = (0..4)
            .map(|index| ginka_protocol::WorkspaceId(format!("comet/work-{index}")))
            .collect::<Vec<_>>();
        let order = |open| {
            row_indices(&entries(&groups, true, &[2, 3], open))
                .into_iter()
                .map(|index| ids[index].clone())
                .collect::<Vec<_>>()
        };
        let visible = order(true);
        assert_eq!(visible, ids);
        let mut selection = crate::folders::Selection::default();
        selection.anchor(&ids[1]);
        selection.range(&ids[3], &visible);
        assert_eq!(selection.targets(&visible), ids[1..]);
        assert!(crate::folders::valid_targets(
            &selection.targets(&visible),
            &visible
        ));
        selection.anchor(&ids[3]);
        selection.retain(&order(false));
        assert_eq!(selection.targets(&visible), ids[1..2]);
        selection.retain(&visible);
        assert_eq!(selection.targets(&visible), ids[1..2]);
        // A hidden archived anchor must not extend the next range.
        selection.range(&ids[0], &visible);
        assert_eq!(selection.targets(&visible), ids[..1]);
        selection.toggle(&ids[2]);
        assert_eq!(
            selection.targets(&visible),
            [ids[0].clone(), ids[2].clone()]
        );
        assert!(row_indices(&[Entry::ArchivedHeading, Entry::ArchivedFoot]).is_empty());
    }

    #[test]
    fn a_change_is_found_where_it_starts() {
        let before = [Entry::Row(0), Entry::Row(1), Entry::ArchivedHeading];
        assert_eq!(first_difference(&before, &before), None);
        assert_eq!(
            first_difference(
                &before,
                &[Entry::Row(0), Entry::Row(2), Entry::ArchivedHeading]
            ),
            Some(1)
        );
        assert_eq!(first_difference(&before, &before[..2]), Some(2), "shorter");
        assert_eq!(first_difference(&before[..1], &before), Some(1), "longer");
    }
}
