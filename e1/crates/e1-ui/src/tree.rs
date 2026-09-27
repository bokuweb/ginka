//! A repository's paths, folded into a file tree.
//!
//! GitHub hands over every path in one flat list. A reader wants folders, so
//! the paths are folded once here into nodes, and flattened again into the
//! rows the window draws. The view then holds indices rather than strings,
//! and folding a directory is a set insert, not a walk of the repository.

use std::collections::{HashMap, HashSet};

/// One entry of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The path from the repository root.
    pub path: String,
    /// The last segment, which is what a row shows.
    pub name: String,
    /// A directory rather than a file.
    pub dir: bool,
    /// How deep it sits, from zero at the root.
    pub depth: usize,
    /// What it holds: directories first, then files, each alphabetical.
    pub children: Vec<usize>,
}

/// A repository's file tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    nodes: Vec<Node>,
    roots: Vec<usize>,
}

impl Tree {
    /// Fold file paths into a tree, creating the directories they imply.
    ///
    /// The directories come from the paths rather than from GitHub's own
    /// directory entries, because a tree built that way cannot end up
    /// showing a folder that holds nothing a reader can open.
    pub fn build<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut nodes: Vec<Node> = Vec::new();
        let mut roots: Vec<usize> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for path in paths {
            let mut parent: Option<usize> = None;
            let mut walked = String::new();
            let mut parts = path
                .as_ref()
                .split('/')
                .filter(|part| !part.is_empty())
                .peekable();
            let mut depth = 0;
            while let Some(part) = parts.next() {
                if !walked.is_empty() {
                    walked.push('/');
                }
                walked.push_str(part);
                let at = match seen.get(&walked) {
                    Some(at) => *at,
                    None => {
                        let at = nodes.len();
                        nodes.push(Node {
                            path: walked.clone(),
                            name: part.to_string(),
                            dir: parts.peek().is_some(),
                            depth,
                            children: Vec::new(),
                        });
                        seen.insert(walked.clone(), at);
                        match parent {
                            Some(parent) => nodes[parent].children.push(at),
                            None => roots.push(at),
                        }
                        at
                    }
                };
                parent = Some(at);
                depth += 1;
            }
        }
        let mut tree = Self { nodes, roots };
        tree.sort();
        tree
    }

    /// Directories before files, then by name, the way GitHub lists them.
    fn sort(&mut self) {
        let keys: Vec<(bool, String)> = self
            .nodes
            .iter()
            .map(|node| (!node.dir, node.name.to_lowercase()))
            .collect();
        let by_key = |a: &usize, b: &usize| keys[*a].cmp(&keys[*b]);
        for at in 0..self.nodes.len() {
            let mut children = std::mem::take(&mut self.nodes[at].children);
            children.sort_by(by_key);
            self.nodes[at].children = children;
        }
        self.roots.sort_by(by_key);
    }

    /// One node, by the index a row carries.
    pub fn node(&self, at: usize) -> Option<&Node> {
        self.nodes.get(at)
    }

    /// How many nodes there are, directories included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the repository had no paths at all.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The nodes on screen, in order, given the directories that are open.
    /// A folded directory hides everything under it.
    pub fn rows(&self, expanded: &HashSet<String>) -> Vec<usize> {
        let mut rows = Vec::with_capacity(self.roots.len());
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(at) = stack.pop() {
            rows.push(at);
            let node = &self.nodes[at];
            if node.dir && expanded.contains(&node.path) {
                stack.extend(node.children.iter().rev().copied());
            }
        }
        rows
    }

    /// Where a path sits in the rows a given expansion produces.
    pub fn row_of(&self, path: &str, expanded: &HashSet<String>) -> Option<usize> {
        self.rows(expanded)
            .into_iter()
            .position(|at| self.nodes[at].path == path)
    }
}

/// Every directory that holds a path, outermost first: what has to be
/// unfolded for the path to be on screen.
pub fn ancestors(path: &str) -> Vec<String> {
    let mut walked = String::new();
    let mut all = Vec::new();
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(part);
        all.push(walked.clone());
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn paths_fold_into_directories_and_files() {
        let tree = Tree::build(["src/main.rs", "src/ui/theme.rs", "README.md"]);
        let rows: Vec<&str> = tree
            .rows(&expanded(&[]))
            .iter()
            .map(|at| tree.node(*at).unwrap().path.as_str())
            .collect();
        // Folded, only the root level shows — the directory before the file.
        assert_eq!(rows, ["src", "README.md"]);
        let src = tree.node(tree.rows(&expanded(&[]))[0]).unwrap();
        assert!(src.dir);
        assert_eq!(src.depth, 0);
    }

    #[test]
    fn unfolding_a_directory_shows_what_it_holds() {
        let tree = Tree::build(["src/main.rs", "src/ui/theme.rs", "README.md"]);
        let rows: Vec<&str> = tree
            .rows(&expanded(&["src"]))
            .iter()
            .map(|at| tree.node(*at).unwrap().path.as_str())
            .collect();
        assert_eq!(rows, ["src", "src/ui", "src/main.rs", "README.md"]);
        let rows: Vec<&str> = tree
            .rows(&expanded(&["src", "src/ui"]))
            .iter()
            .map(|at| tree.node(*at).unwrap().path.as_str())
            .collect();
        assert_eq!(
            rows,
            [
                "src",
                "src/ui",
                "src/ui/theme.rs",
                "src/main.rs",
                "README.md"
            ]
        );
        assert_eq!(tree.node(1).map(|node| node.depth), Some(1));
    }

    #[test]
    fn a_directory_appears_once_however_many_files_it_holds() {
        let tree = Tree::build(["a/b/one.rs", "a/b/two.rs", "a/three.rs"]);
        assert_eq!(tree.len(), 5, "a, a/b, two files under b, one under a");
        assert!(!tree.is_empty());
    }

    #[test]
    fn the_ancestors_of_a_path_are_the_directories_over_it() {
        assert_eq!(ancestors("src/ui/theme.rs"), ["src", "src/ui"]);
        assert!(ancestors("README.md").is_empty());
    }

    #[test]
    fn a_row_can_be_found_by_its_path() {
        let tree = Tree::build(["src/main.rs", "README.md"]);
        assert_eq!(tree.row_of("src/main.rs", &expanded(&["src"])), Some(1));
        assert_eq!(tree.row_of("src/main.rs", &expanded(&[])), None);
    }
}
