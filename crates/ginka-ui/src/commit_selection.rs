//! Explicit file scope for commits, independent of diff filtering and staging.

use ginka_protocol::model::{ChangeKind, FileChange};
use std::collections::BTreeSet;

/// Selected paths for one workspace's commit draft.
///
/// Deselecting the last file keeps selection mode active: an empty selection
/// must never silently fall back to committing the index or entire workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitSelection {
    active: bool,
    selected: BTreeSet<String>,
}

impl CommitSelection {
    /// Whether an explicit file scope overrides the usual staged/all scope.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Whether this file is checked, including its duplicate in another section.
    pub fn contains(&self, path: &str) -> bool {
        self.selected.contains(path)
    }

    /// Enter selection mode and set a file's checked state.
    pub fn set(&mut self, path: String, checked: bool) {
        self.active = true;
        if checked {
            self.selected.insert(path);
        } else {
            self.selected.remove(&path);
        }
    }

    /// Return to the normal staged/all scope, discarding the explicit selection.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Count selected files still present in either current changes section.
    pub fn count<'a>(&self, files: impl IntoIterator<Item = &'a FileChange>) -> usize {
        files
            .into_iter()
            .filter(|file| self.contains(&file.path))
            .map(|file| &file.path)
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Resolve literal paths against the complete, unfiltered current changes.
    ///
    /// A rename includes its old path so the deletion accompanies its addition.
    /// Files no longer changed do not widen the scope; callers must refuse an
    /// active selection whose resolved paths are empty.
    pub fn paths<'a>(&self, files: impl IntoIterator<Item = &'a FileChange>) -> Vec<String> {
        let mut paths = BTreeSet::new();
        for file in files {
            if self.contains(&file.path) {
                paths.insert(file.path.clone());
                if file.kind == ChangeKind::Renamed
                    && let Some(old) = &file.old_path
                {
                    paths.insert(old.clone());
                }
            }
        }
        paths.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, old_path: Option<&str>) -> FileChange {
        FileChange {
            path: path.into(),
            old_path: old_path.map(str::to_string),
            kind: if old_path.is_some() {
                ChangeKind::Renamed
            } else {
                ChangeKind::Modified
            },
            added: 1,
            removed: 0,
            binary: false,
            hunks: Vec::new(),
        }
    }

    #[test]
    fn staged_and_unstaged_rows_share_one_selection() {
        let files = [file("a", None), file("a", None), file("b", None)];
        let mut selection = CommitSelection::default();
        selection.set("a".into(), true);
        assert_eq!(selection.count(&files), 1);
        assert_eq!(selection.paths(&files), vec!["a"]);
    }

    #[test]
    fn renames_include_both_literal_paths_and_count_as_one_file() {
        let files = [file("new", Some("old")), file("new", None)];
        let mut selection = CommitSelection::default();
        selection.set("new".into(), true);
        assert_eq!(selection.count(&files), 1);
        assert_eq!(selection.paths(&files), vec!["new", "old"]);
    }

    #[test]
    fn deselecting_the_last_file_keeps_an_empty_explicit_scope() {
        let files = [file("a", None), file("b", None)];
        let mut selection = CommitSelection::default();
        selection.set("a".into(), true);
        selection.set("a".into(), false);
        assert!(selection.active());
        assert!(selection.paths(&files).is_empty());
    }

    #[test]
    fn vanished_files_do_not_fall_back_to_unrelated_changes() {
        let mut selection = CommitSelection::default();
        selection.set("gone".into(), true);
        assert_eq!(selection.count(&[file("b", None)]), 0);
        assert!(selection.active());
        assert!(selection.paths(&[file("b", None)]).is_empty());
    }

    #[test]
    fn clearing_selection_restores_the_usual_scope() {
        let mut selection = CommitSelection::default();
        selection.set("a".into(), true);
        selection.clear();
        assert!(!selection.active());
        assert!(!selection.contains("a"));
    }
}
