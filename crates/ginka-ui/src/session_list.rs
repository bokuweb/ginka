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
    let mut entries = Vec::new();
    for group in groups {
        if headings {
            entries.push(Entry::Project(group.name.clone()));
        }
        entries.extend(group.rows.iter().map(|(index, _)| Entry::Row(*index)));
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
