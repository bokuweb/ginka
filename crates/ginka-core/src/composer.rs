//! What the composer offers to complete, and where the offers come from.
//!
//! Two rules make this a domain module rather than view code. A provider
//! defines half of its own slash commands, so a list read only from disk shows
//! the user something that is not true (`docs/roadmap.md` §3.3 N4). And the
//! `@file` index is bounded before it is built, because the repository that
//! breaks an unbounded walk is generated, enormous and someone's daily driver.
//!
//! Everything here is plain data work over text and paths, so the popup itself
//! stays a pure view: discovery walks the filesystem and must run off the UI
//! thread, but filtering a built index is cheap enough for a keystroke.

use anyhow::{Context, Result};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::git::Git;

/// Rows one filter pass returns. The popup shows a screenful and the keyboard
/// walks the rest; past this the tail is noise rather than choice.
pub const FILTER_CAP: usize = 64;

/// What the caret is currently asking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// A slash command. `start` is the byte offset of the `/`.
    Slash { query: String, start: usize },
    /// A file mention. `start` is the byte offset of the `@`.
    File { query: String, start: usize },
}

impl Trigger {
    /// Read the trigger under the caret, if there is one.
    ///
    /// A slash only counts at the start of a line — otherwise every URL and
    /// every path in a prompt would open the command list. An `@` only counts
    /// after whitespace, which is what keeps an email address from being read
    /// as a file mention.
    pub fn detect(text: &str, cursor: usize) -> Option<Self> {
        let cursor = cursor.min(text.len());
        if !text.is_char_boundary(cursor) {
            return None;
        }
        let before = &text[..cursor];
        let token_start = before
            .rfind(|c: char| c.is_whitespace())
            .map_or(0, |index| index + 1);
        let token = &before[token_start..];

        if let Some(query) = token.strip_prefix('/') {
            let at_line_start = token_start == 0 || before[..token_start].ends_with('\n');
            if at_line_start {
                return Some(Self::Slash {
                    query: query.to_string(),
                    start: token_start,
                });
            }
        }
        if let Some(query) = token.strip_prefix('@') {
            return Some(Self::File {
                query: query.to_string(),
                start: token_start,
            });
        }
        None
    }

    pub fn query(&self) -> &str {
        match self {
            Self::Slash { query, .. } | Self::File { query, .. } => query,
        }
    }
}

/// Who defined a command, which is also the order of precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    /// Reported by the running agent itself.
    Provider,
    /// Defined in the project's own command directory.
    Project,
    /// Defined in the user's home directory.
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub scope: CommandScope,
}

impl SlashCommand {
    pub fn new(name: impl Into<String>, scope: CommandScope) -> Self {
        Self {
            name: name.into(),
            description: None,
            scope,
        }
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Combine what the agent reported with what is on disk.
///
/// The provider's own list wins on a name collision: it is the live truth
/// about the session that is actually running, where a file on disk may be a
/// copy left behind by another tool.
pub fn merge_commands(
    from_provider: Vec<SlashCommand>,
    from_disk: Vec<SlashCommand>,
) -> Vec<SlashCommand> {
    let mut merged = from_provider;
    for command in from_disk {
        if !merged.iter().any(|existing| existing.name == command.name) {
            merged.push(command);
        }
    }
    merged.sort_by(|left, right| left.name.cmp(&right.name));
    merged
}

pub fn filter_commands<'a>(commands: &'a [SlashCommand], query: &str) -> Vec<&'a SlashCommand> {
    rank(commands, query, Config::DEFAULT, |command| {
        command.name.as_str()
    })
}

pub fn filter_files<'a>(paths: &'a [String], query: &str) -> Vec<&'a String> {
    // Path scoring weighs the segment after the last separator, which is where
    // people aim when they type a file name.
    rank(paths, query, Config::DEFAULT.match_paths(), |path| {
        path.as_str()
    })
}

fn rank<'a, T>(
    items: &'a [T],
    query: &str,
    config: Config,
    key: impl Fn(&T) -> &str,
) -> Vec<&'a T> {
    if query.is_empty() {
        return items.iter().take(FILTER_CAP).collect();
    }
    let mut matcher = Matcher::new(config);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<Ranked<'a, T>> = Vec::new();
    let mut buffer = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let candidate = key(item);
        let haystack = Utf32Str::new(candidate, &mut buffer);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            scored.push(Ranked {
                exact: candidate.eq_ignore_ascii_case(query),
                score,
                length: candidate.chars().count(),
                index,
                item,
            });
        }
    }
    // What the user typed is never buried under a longer thing that happens to
    // contain it: typing `rev` offers `/rev` before `/review-diff`. Ties keep
    // the input order, so the list does not reshuffle between keystrokes.
    scored.sort_by(|left, right| {
        right
            .exact
            .cmp(&left.exact)
            .then(right.score.cmp(&left.score))
            .then(left.length.cmp(&right.length))
            .then(left.index.cmp(&right.index))
    });
    scored
        .into_iter()
        .take(FILTER_CAP)
        .map(|ranked| ranked.item)
        .collect()
}

struct Ranked<'a, T> {
    exact: bool,
    score: u32,
    length: usize,
    index: usize,
    item: &'a T,
}

/// The set of paths `@` can mention, built once and filtered per keystroke.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    paths: Vec<String>,
    truncated: bool,
}

impl FileIndex {
    /// Build the index for a workspace, stopping at `cap` entries.
    ///
    /// Inside a repository the list comes from git, which already knows what
    /// is ignored — no second ignore implementation to disagree with the one
    /// the user's tools use. Outside one it is a bounded walk.
    pub fn build(root: &Path, cap: usize) -> Result<Self> {
        let git = Git::new(root);
        let listing = git.query(&[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
        ])?;

        let mut paths: Vec<String> = match listing {
            Some(listing) => listing.lines().map(str::to_string).collect(),
            None => walk(root, cap + 1)?,
        };
        paths.sort();

        let truncated = paths.len() > cap;
        paths.truncate(cap);
        Ok(Self { paths, truncated })
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Whether the cap was reached, so the UI can say the list is partial
    /// rather than implying the repository is small.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// A depth-first walk for folders that are not repositories. Hidden
/// directories are skipped: nobody mentions a file inside `.git`.
fn walk(root: &Path, limit: usize) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(directory) = stack.pop() {
        if found.len() >= limit {
            break;
        }
        let entries = std::fs::read_dir(&directory)
            .with_context(|| format!("indexing {}", directory.display()))?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() => {
                    if let Ok(relative) = path.strip_prefix(root) {
                        found.push(relative.to_string_lossy().replace('\\', "/"));
                    }
                    if found.len() >= limit {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(found)
}
