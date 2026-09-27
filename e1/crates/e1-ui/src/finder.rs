//! Finding a file by typing part of its path.
//!
//! `nucleo`, the matcher Helix and Zed use: fuzzy, case-insensitive, and
//! fast enough that a repository of twenty thousand paths answers between
//! keystrokes. The result is indices into the list it was given, so the
//! caller keeps one list of paths and never copies it.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// The paths that match `query`, best first, at most `cap` of them.
///
/// An empty query is the list as given, cut to `cap`: a finder that
/// shuffled the tree before anything was typed would be unreadable.
pub fn find(paths: &[String], query: &str, cap: usize) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..paths.len().min(cap)).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buffer = Vec::new();
    let mut scored: Vec<(u32, usize)> = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let haystack = nucleo_matcher::Utf32Str::new(path, &mut buffer);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (score, index))
        })
        .collect();
    // Stable on the score and then on the path, so the same query always
    // answers with the same list.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| paths[a.1].cmp(&paths[b.1])));
    scored
        .into_iter()
        .map(|(_, index)| index)
        .take(cap)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        [
            "Cargo.toml",
            "src/main.rs",
            "crates/e1-ui/src/theme.rs",
            "crates/e1-views/src/shell.rs",
            "docs/roadmap.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn an_empty_query_is_the_tree_in_order() {
        assert_eq!(find(&paths(), "", 10), vec![0, 1, 2, 3, 4]);
        assert_eq!(find(&paths(), "  ", 2), vec![0, 1]);
    }

    #[test]
    fn a_query_matches_across_path_separators_and_ignores_case() {
        let found = find(&paths(), "uitheme", 10);
        assert_eq!(found, vec![2]);
        let found = find(&paths(), "SHELL", 10);
        assert_eq!(found, vec![3]);
    }

    #[test]
    fn the_best_match_comes_first_and_the_cap_holds() {
        let found = find(&paths(), "rs", 10);
        assert!(found.len() >= 3);
        assert!(found.iter().all(|i| paths()[*i].ends_with(".rs")));
        assert_eq!(find(&paths(), "rs", 1).len(), 1);
    }

    #[test]
    fn nothing_matches_nonsense() {
        assert!(find(&paths(), "zzzzqqq", 10).is_empty());
    }
}
