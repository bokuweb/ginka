//! View-state decisions for the workspace file tree.

use ginka_protocol::model::FileEntry;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum number of files retained by the tree in one workspace.
pub const TREE_FILE_LIMIT: usize = 2_000;

/// Apply the tree's memory bound while preserving the daemon's stable order.
pub fn bounded_catalogue(mut files: Vec<FileEntry>, limit: usize) -> (Vec<FileEntry>, bool) {
    let truncated = files.len() > limit;
    files.truncate(limit);
    (files, truncated)
}

/// Whether a visible tree row opens a directory or a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeRowKind {
    /// A directory whose children can be expanded in place.
    Directory,
    /// A file that opens in the editor surface.
    File,
}

/// One visible row after collapsed subtrees have been omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Worktree-relative path used by open and toggle actions.
    pub path: String,
    /// Last path component shown in the row.
    pub name: String,
    /// Directory or file behaviour.
    pub kind: TreeRowKind,
    /// Zero-based nesting used for indentation.
    pub depth: usize,
    /// Whether a directory currently exposes its children.
    pub expanded: bool,
}

/// Expansion state retained while search temporarily replaces the tree.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileTree {
    expanded: BTreeSet<String>,
}

impl FileTree {
    /// Expand a collapsed directory or collapse an expanded one.
    pub fn toggle(&mut self, path: &str) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
    }

    /// Fold a flat, stable file catalogue into visible directory-first rows.
    pub fn rows(&self, files: &[FileEntry]) -> Vec<TreeRow> {
        let mut root = Node::default();
        for file in files {
            root.insert(file);
        }
        let mut rows = Vec::new();
        root.append_rows("", 0, &self.expanded, &mut rows);
        rows
    }
}

#[derive(Default)]
struct Node {
    directories: BTreeMap<String, Node>,
    files: BTreeMap<String, String>,
}

impl Node {
    fn insert(&mut self, file: &FileEntry) {
        let mut segments = file.path.split('/').filter(|segment| !segment.is_empty());
        let Some(first) = segments.next() else {
            return;
        };
        let mut collected = vec![first];
        collected.extend(segments);
        let Some((name, directories)) = collected.split_last() else {
            return;
        };
        let mut node = self;
        for directory in directories {
            node = node
                .directories
                .entry((*directory).to_string())
                .or_default();
        }
        node.files.insert((*name).to_string(), file.path.clone());
    }

    fn append_rows(
        &self,
        parent: &str,
        depth: usize,
        expanded: &BTreeSet<String>,
        rows: &mut Vec<TreeRow>,
    ) {
        for (name, directory) in &self.directories {
            let path = join(parent, name);
            let is_expanded = expanded.contains(&path);
            rows.push(TreeRow {
                path: path.clone(),
                name: name.clone(),
                kind: TreeRowKind::Directory,
                depth,
                expanded: is_expanded,
            });
            if is_expanded {
                directory.append_rows(&path, depth + 1, expanded, rows);
            }
        }
        for (name, path) in &self.files {
            rows.push(TreeRow {
                path: path.clone(),
                name: name.clone(),
                kind: TreeRowKind::File,
                depth,
                expanded: false,
            });
        }
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::model::FileEntry;

    fn files(paths: &[&str]) -> Vec<FileEntry> {
        paths
            .iter()
            .map(|path| FileEntry {
                path: (*path).into(),
                name: path.rsplit('/').next().unwrap_or(path).to_string(),
                score: 0,
            })
            .collect()
    }

    #[test]
    fn directories_precede_files_and_only_expanded_children_are_visible() {
        let files = files(&[
            "Cargo.toml",
            "src/lib.rs",
            "src/nested/mod.rs",
            "tests/smoke.rs",
        ]);
        let mut tree = FileTree::default();

        assert_eq!(
            tree.rows(&files)
                .iter()
                .map(|row| (row.path.as_str(), row.kind, row.depth))
                .collect::<Vec<_>>(),
            [
                ("src", TreeRowKind::Directory, 0),
                ("tests", TreeRowKind::Directory, 0),
                ("Cargo.toml", TreeRowKind::File, 0),
            ]
        );

        tree.toggle("src");
        assert_eq!(
            tree.rows(&files)
                .iter()
                .map(|row| (row.path.as_str(), row.kind, row.depth))
                .collect::<Vec<_>>(),
            [
                ("src", TreeRowKind::Directory, 0),
                ("src/nested", TreeRowKind::Directory, 1),
                ("src/lib.rs", TreeRowKind::File, 1),
                ("tests", TreeRowKind::Directory, 0),
                ("Cargo.toml", TreeRowKind::File, 0),
            ]
        );
    }

    #[test]
    fn collapsing_a_parent_keeps_descendant_expansion_for_when_it_reopens() {
        let files = files(&["src/nested/mod.rs"]);
        let mut tree = FileTree::default();
        tree.toggle("src");
        tree.toggle("src/nested");
        tree.toggle("src");
        assert_eq!(tree.rows(&files).len(), 1);

        tree.toggle("src");
        assert_eq!(
            tree.rows(&files)
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["src", "src/nested", "src/nested/mod.rs"]
        );
    }

    #[test]
    fn the_catalogue_reports_when_the_tree_bound_cut_it_short() {
        let files = files(&["a", "b", "c"]);
        let (bounded, truncated) = bounded_catalogue(files.clone(), 2);
        assert_eq!(bounded.len(), 2);
        assert!(truncated);

        let (complete, truncated) = bounded_catalogue(files, 3);
        assert_eq!(complete.len(), 3);
        assert!(!truncated);
    }
}
