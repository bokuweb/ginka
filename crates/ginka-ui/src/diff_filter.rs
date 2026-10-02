//! File-path filtering for the Git surface.
//!
//! Filtering only changes which rows are drawn. Review actions still use the
//! original daemon-provided changes and their exact paths and hunk headers.

use ginka_protocol::model::FileChange;
use std::collections::BTreeSet;

/// A case-insensitive, whitespace-separated path query for diff file rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffFilter {
    /// The text typed by the reader; an empty query includes every file.
    pub query: String,
}

impl DiffFilter {
    /// Match every term against the old or new path of a changed file.
    pub fn matches(&self, file: &FileChange) -> bool {
        let path = file.path.to_lowercase();
        let old_path = file.old_path.as_deref().unwrap_or_default().to_lowercase();
        self.query.split_whitespace().all(|term| {
            path.contains(&term.to_lowercase()) || old_path.contains(&term.to_lowercase())
        })
    }

    /// Count distinct matching paths across staged and unstaged sections.
    pub fn visible_count<'a>(&self, files: impl IntoIterator<Item = &'a FileChange>) -> usize {
        files
            .into_iter()
            .filter(|file| self.matches(file))
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>()
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::model::ChangeKind;

    fn file(path: &str, old_path: Option<&str>) -> FileChange {
        FileChange {
            path: path.into(),
            old_path: old_path.map(str::to_string),
            kind: ChangeKind::Modified,
            added: 1,
            removed: 0,
            binary: false,
            hunks: Vec::new(),
        }
    }

    #[test]
    fn empty_query_includes_every_file() {
        assert!(DiffFilter::default().matches(&file("src/main.rs", None)));
    }

    #[test]
    fn all_terms_match_case_insensitively_across_rename_paths() {
        let renamed = file("src/new_name.rs", Some("legacy/old_name.rs"));
        assert!(
            DiffFilter {
                query: "LEGACY new".into()
            }
            .matches(&renamed)
        );
        assert!(
            !DiffFilter {
                query: "legacy other".into()
            }
            .matches(&renamed)
        );
    }

    #[test]
    fn staged_and_unstaged_copies_count_once() {
        let files = [
            file("src/main.rs", None),
            file("src/main.rs", None),
            file("tests/main.rs", None),
        ];
        let filter = DiffFilter {
            query: "src main".into(),
        };
        assert_eq!(filter.visible_count(&files), 1);
    }

    #[test]
    fn unmatched_query_has_no_visible_files() {
        let files = [file("src/main.rs", None)];
        let filter = DiffFilter {
            query: "missing".into(),
        };
        assert_eq!(filter.visible_count(&files), 0);
    }
}
