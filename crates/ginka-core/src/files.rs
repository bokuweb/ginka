//! The files in a workspace, and finding one by typing part of its name.
//!
//! Listing goes through `git ls-files` rather than walking the directory: it
//! honours `.gitignore` for free, which is the difference between offering the
//! user their own source files and offering them `node_modules`. Untracked
//! files are included — a file the agent wrote a minute ago is exactly the one
//! being reached for.
//!
//! Matching is `nucleo`, the matcher the roadmap picked (§4.3): a path is
//! matched by typing any subsequence of it, and the ranking understands that a
//! hit in the file name beats one in a directory three levels up.

use crate::git;
use anyhow::Result;
use ginka_protocol::model::FileEntry;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::Path;

/// How many paths a search answers with by default.
///
/// A menu is read, not scrolled: past this the user is better served by typing
/// another character than by a longer list.
pub const DEFAULT_LIMIT: usize = 30;

/// Every file in the worktree that git would show, tracked or not.
pub fn list(worktree: &Path) -> Result<Vec<String>> {
    // `--others --exclude-standard` adds untracked files while still obeying
    // the ignore rules; `--cached` keeps the tracked ones. Deduplicated,
    // because a file can be both when it is staged and modified.
    let listed = git::ls_files(worktree)?;
    let mut paths: Vec<String> = listed;
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

/// The paths that match `query`, best first.
///
/// An empty query is not a match of everything in an arbitrary order: it is
/// the start of the list, which is what a picker shows before anything is
/// typed.
pub fn search(paths: &[String], query: &str, limit: usize) -> Vec<FileEntry> {
    let query = query.trim();
    if query.is_empty() {
        return paths
            .iter()
            .take(limit)
            .map(|path| entry(path, 0))
            .collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, &String)> = pattern
        .match_list(paths.iter(), &mut matcher)
        .into_iter()
        .map(|(path, score)| (score, path))
        .collect();
    // Stable on the score, then on the path, so the same query always answers
    // in the same order.
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(score, path)| entry(path, score))
        .collect()
}

/// One path, with the part a list shows separately.
fn entry(path: &str, score: u32) -> FileEntry {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    FileEntry {
        path: path.to_string(),
        name,
        score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        [
            "src/main.rs",
            "src/shell.rs",
            "crates/ginka-core/src/service.rs",
            "crates/ginka-core/src/session.rs",
            "docs/ui.md",
            "README.md",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    #[test]
    fn typing_part_of_a_name_finds_it() {
        let found = search(&paths(), "service", 10);
        assert_eq!(found[0].path, "crates/ginka-core/src/service.rs");
        assert_eq!(found[0].name, "service.rs", "a list shows the name apart");
    }

    #[test]
    fn a_subsequence_is_enough() {
        // What makes a picker usable: `srvc` for `service.rs`.
        let found = search(&paths(), "srvc", 10);
        assert!(
            found.iter().any(|entry| entry.path.ends_with("service.rs")),
            "{found:?}"
        );
    }

    #[test]
    fn a_hit_in_the_name_beats_one_in_a_directory() {
        // `ui` appears in `docs/ui.md` as the name and in nothing else here;
        // the ranking must not bury it under a path that merely contains the
        // letters.
        let found = search(&paths(), "ui", 10);
        assert_eq!(found[0].path, "docs/ui.md", "{found:?}");
    }

    #[test]
    fn nothing_typed_is_the_start_of_the_list_rather_than_a_shuffle() {
        let found = search(&paths(), "", 3);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].path, "src/main.rs");
    }

    #[test]
    fn a_query_that_matches_nothing_answers_with_nothing() {
        assert!(search(&paths(), "zzzz", 10).is_empty());
    }

    #[test]
    fn the_list_is_bounded_because_a_menu_is_read_not_scrolled() {
        let many: Vec<String> = (0..500).map(|index| format!("file{index}.rs")).collect();
        assert_eq!(search(&many, "file", 10).len(), 10);
    }

    #[test]
    fn the_same_query_always_answers_in_the_same_order() {
        let first = search(&paths(), "rs", 10);
        let again = search(&paths(), "rs", 10);
        assert_eq!(first, again);
    }
}
