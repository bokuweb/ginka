//! View-state decisions for the workspace file tree.

use ginka_protocol::model::FileEntry;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

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

/// Keyboard operations over the currently visible, directory-first rows.
#[derive(Debug, Clone, Copy)]
pub enum TreeNavigation {
    /// Previous row, stopping at the beginning.
    Previous,
    /// Next row, stopping at the end.
    Next,
    /// First visible row.
    First,
    /// Last visible row.
    Last,
    /// One viewport toward the beginning.
    PageUp,
    /// One viewport toward the end.
    PageDown,
    /// Collapse a directory, otherwise select its parent.
    Left,
    /// Expand a directory, otherwise select its first child.
    Right,
    /// Toggle a directory or open a file.
    Activate,
}

/// Expansion and path-based selection retained while search replaces the tree.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileTree {
    expanded: BTreeSet<String>,
    selected: Option<String>,
    prefix: String,
    typed_at: Option<Instant>,
}

impl FileTree {
    /// Selected workspace-relative path, independent of row ordering.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Select a clicked row and end any keyboard prefix search.
    pub fn select(&mut self, path: &str) {
        self.selected = Some(path.into());
        self.prefix.clear();
        self.typed_at = None;
    }

    /// Keep selection visible after a refresh or a collapsed ancestor.
    pub fn reconcile(&mut self, files: &[FileEntry]) {
        let rows = self.rows(files);
        let mut candidate = self.selected.as_deref();
        while let Some(path) = candidate {
            if rows.iter().any(|row| row.path == path) {
                self.selected = Some(path.into());
                return;
            }
            candidate = path.rsplit_once('/').map(|(parent, _)| parent);
        }
        self.selected = rows.first().map(|row| row.path.clone());
    }

    /// Move or activate selection; only activating a file returns an open path.
    pub fn navigate(
        &mut self,
        files: &[FileEntry],
        action: TreeNavigation,
        page_size: usize,
    ) -> Option<String> {
        self.prefix.clear();
        self.typed_at = None;
        self.reconcile(files);
        let rows = self.rows(files);
        let index = rows
            .iter()
            .position(|row| Some(row.path.as_str()) == self.selected())?;
        let row = &rows[index];
        let page = page_size.max(1);
        let next = match action {
            TreeNavigation::Previous => Some(index.saturating_sub(1)),
            TreeNavigation::Next => Some(index.saturating_add(1).min(rows.len() - 1)),
            TreeNavigation::First => Some(0),
            TreeNavigation::Last => Some(rows.len() - 1),
            TreeNavigation::PageUp => Some(index.saturating_sub(page)),
            TreeNavigation::PageDown => Some(index.saturating_add(page).min(rows.len() - 1)),
            TreeNavigation::Right if row.kind == TreeRowKind::Directory => {
                if row.expanded {
                    rows.get(index + 1)
                        .filter(|child| child.depth > row.depth)
                        .map(|_| index + 1)
                } else {
                    self.toggle(&row.path);
                    None
                }
            }
            TreeNavigation::Left => {
                if row.expanded {
                    self.toggle(&row.path);
                    None
                } else {
                    row.path
                        .rsplit_once('/')
                        .and_then(|(parent, _)| rows.iter().position(|row| row.path == parent))
                }
            }
            TreeNavigation::Activate => {
                if row.kind == TreeRowKind::File {
                    return Some(row.path.clone());
                }
                self.toggle(&row.path);
                None
            }
            TreeNavigation::Right => None,
        };
        if let Some(next) = next {
            self.selected = Some(rows[next].path.clone());
        }
        self.reconcile(files);
        None
    }

    /// Find a visible filename by Unicode prefix; repeated letters cycle matches.
    /// A one-second pause begins a new prefix instead of extending the old one.
    /// At most 128 lowercase characters are retained, including pasted input.
    pub fn type_prefix(&mut self, files: &[FileEntry], text: &str, now: Instant) -> bool {
        if text.is_empty() || text.chars().any(char::is_control) {
            return false;
        }
        self.reconcile(files);
        let text: String = text.to_lowercase().chars().take(128).collect();
        let continuing = self
            .typed_at
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(1));
        let cycling = continuing && self.prefix == text && text.chars().count() == 1;
        if !continuing || cycling {
            self.prefix = text;
        } else if self.prefix.chars().count() + text.chars().count() <= 128 {
            self.prefix.push_str(&text);
        }
        self.typed_at = Some(now);
        let rows = self.rows(files);
        if rows.is_empty() {
            return true;
        }
        let current = rows
            .iter()
            .position(|row| Some(row.path.as_str()) == self.selected())
            .unwrap_or(0);
        let start = if continuing && !cycling {
            current
        } else {
            (current + 1) % rows.len()
        };
        if let Some(row) = (0..rows.len())
            .map(|offset| &rows[(start + offset) % rows.len()])
            .find(|row| row.name.to_lowercase().starts_with(&self.prefix))
        {
            self.selected = Some(row.path.clone());
        }
        true
    }

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

    #[test]
    fn keyboard_walks_children_parents_and_opens_only_files() {
        let files = files(&["src/nested/mod.rs", "src/lib.rs", "README.md"]);
        let mut tree = FileTree::default();
        assert_eq!(tree.navigate(&files, TreeNavigation::Right, 5), None);
        assert_eq!(tree.selected(), Some("src"));
        tree.navigate(&files, TreeNavigation::Right, 5);
        assert_eq!(tree.selected(), Some("src/nested"));
        tree.navigate(&files, TreeNavigation::Activate, 5);
        tree.navigate(&files, TreeNavigation::Right, 5);
        assert_eq!(tree.selected(), Some("src/nested/mod.rs"));
        assert_eq!(
            tree.navigate(&files, TreeNavigation::Activate, 5),
            Some("src/nested/mod.rs".into())
        );
        tree.navigate(&files, TreeNavigation::Left, 5);
        assert_eq!(tree.selected(), Some("src/nested"));
        tree.navigate(&files, TreeNavigation::Left, 5);
        assert!(!tree.rows(&files)[1].expanded);
        tree.navigate(&files, TreeNavigation::Left, 5);
        assert_eq!(tree.selected(), Some("src"));
    }

    #[test]
    fn navigation_clamps_pages_and_handles_an_empty_catalogue() {
        let files = files(&["a", "b", "c", "d", "e"]);
        let mut tree = FileTree::default();
        tree.navigate(&files, TreeNavigation::PageDown, 3);
        assert_eq!(tree.selected(), Some("d"));
        tree.navigate(&files, TreeNavigation::PageDown, 3);
        assert_eq!(tree.selected(), Some("e"));
        tree.navigate(&files, TreeNavigation::PageUp, 3);
        assert_eq!(tree.selected(), Some("b"));
        tree.navigate(&files, TreeNavigation::First, 3);
        tree.navigate(&files, TreeNavigation::Previous, 3);
        assert_eq!(tree.selected(), Some("a"));
        tree.navigate(&files, TreeNavigation::Last, 3);
        tree.navigate(&files, TreeNavigation::Next, 3);
        assert_eq!(tree.selected(), Some("e"));
        assert_eq!(tree.navigate(&[], TreeNavigation::Activate, 0), None);
        assert_eq!(tree.selected(), None);
    }

    #[test]
    fn catalogue_changes_keep_paths_and_collapsing_selects_visible_parent() {
        let files = files(&["src/nested/mod.rs", "README.md"]);
        let mut tree = FileTree::default();
        tree.toggle("src");
        tree.toggle("src/nested");
        tree.select("src/nested/mod.rs");
        let reordered = self::files(&["a.txt", "src/nested/mod.rs"]);
        tree.reconcile(&reordered);
        assert_eq!(tree.selected(), Some("src/nested/mod.rs"));
        tree.toggle("src");
        tree.reconcile(&files);
        assert_eq!(tree.selected(), Some("src"));
        tree.reconcile(&self::files(&["other.txt"]));
        assert_eq!(tree.selected(), Some("other.txt"));
    }

    #[test]
    fn filename_prefix_is_case_insensitive_cycles_and_expires() {
        let files = files(&["Alpha", "apricot", "beta", "日本.txt"]);
        let mut tree = FileTree::default();
        let now = std::time::Instant::now();
        tree.reconcile(&files);
        assert!(tree.type_prefix(&files, "A", now));
        assert_eq!(tree.selected(), Some("apricot"));
        tree.type_prefix(&files, "a", now);
        assert_eq!(tree.selected(), Some("Alpha"));
        tree.type_prefix(&files, "p", now);
        assert_eq!(tree.selected(), Some("apricot"));
        tree.type_prefix(&files, "日", now + std::time::Duration::from_secs(2));
        assert_eq!(tree.selected(), Some("日本.txt"));
        assert!(!tree.type_prefix(&files, "\n", now));
    }

    #[test]
    fn pasted_prefix_is_bounded_and_does_not_consume_control_keys() {
        let files = files(&["a.txt"]);
        let mut tree = FileTree::default();
        let now = Instant::now();
        assert!(tree.type_prefix(&files, &"日".repeat(1_000), now));
        assert_eq!(tree.prefix.chars().count(), 128);
        assert!(!tree.type_prefix(&files, "", now));
        assert!(!tree.type_prefix(&files, "a\t", now));
        assert_eq!(tree.prefix.chars().count(), 128);
    }
}
