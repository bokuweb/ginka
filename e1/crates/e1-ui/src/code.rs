//! Syntax highlighting, by tree-sitter.
//!
//! `gpui-component` ships the grammars and Zed's highlight queries for the
//! languages a repository is likely to hold, so nothing is parsed by hand
//! here: a file is handed to the toolkit's highlighter once, and each row
//! asks it for the styles on its own line as it is drawn. Keeping the parse
//! and dropping the styles is what lets the light and dark palettes share
//! one parse, and what keeps a twenty-thousand-line file from building
//! twenty thousand style vectors nobody will look at.

use crate::theme::{ThemeAppearance, Tokens};
use gpui::{App, HighlightStyle};
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::input::Rope;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

/// Past this much text a file is drawn plain. Parsing happens on the
/// window's thread, and a reader would rather have the file now than the
/// colours in a moment.
pub const MAX_BYTES: usize = 1 << 20;

/// How long the parser may run before the file is drawn with whatever it
/// has. A guard against a pathological file, not a normal path.
const BUDGET: Duration = Duration::from_millis(750);

/// The styles on one line, as byte ranges within that line's own text.
pub type LineStyles = Vec<(Range<usize>, HighlightStyle)>;

/// The highlight palette for the window's appearance.
pub fn theme(cx: &App) -> Arc<HighlightTheme> {
    match Tokens::global(cx).appearance {
        ThemeAppearance::Dark => HighlightTheme::default_dark(),
        ThemeAppearance::Light => HighlightTheme::default_light(),
    }
}

/// A parsed file, ready to be drawn a line at a time.
pub struct Code {
    language: &'static str,
    highlighter: SyntaxHighlighter,
    /// Each line's byte range, matching what [`str::lines`] would yield.
    lines: Vec<Range<usize>>,
}

impl Code {
    /// Parse a file for highlighting.
    ///
    /// `None` when there is nothing to be gained: no grammar for the path,
    /// or more text than is worth parsing while the reader waits.
    pub fn parse(path: &str, text: &str) -> Option<Self> {
        let language = language_for(path)?;
        if text.len() > MAX_BYTES {
            return None;
        }
        let mut highlighter = SyntaxHighlighter::new(language);
        highlighter.update(None, &Rope::from(text), Some(BUDGET));
        Some(Self {
            language,
            highlighter,
            lines: line_ranges(text),
        })
    }

    /// What the file was parsed as, for the reader to see.
    pub fn language(&self) -> &'static str {
        self.language
    }

    /// The styles on one line, as ranges within the line's own text.
    ///
    /// Empty means "draw it plain", which is also what an unparsed line
    /// gets: a caller can hand the answer to `StyledText` unconditionally.
    pub fn line(&self, index: usize, theme: &HighlightTheme) -> LineStyles {
        let Some(range) = self.lines.get(index).filter(|range| !range.is_empty()) else {
            return Vec::new();
        };
        self.highlighter
            .styles(range, theme)
            .into_iter()
            .filter_map(|(at, style)| {
                let start = at.start.clamp(range.start, range.end) - range.start;
                let end = at.end.clamp(range.start, range.end) - range.start;
                (start < end).then_some((start..end, style))
            })
            .collect()
    }
}

/// Each line's byte range, split the way [`str::lines`] splits: on `\n`,
/// with a carriage return before it left out, and no empty line invented
/// after a trailing newline.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut at = 0;
    for chunk in text.split_inclusive('\n') {
        let mut end = at + chunk.len();
        if chunk.ends_with('\n') {
            end -= 1;
            if text[at..end].ends_with('\r') {
                end -= 1;
            }
        }
        ranges.push(at..end);
        at += chunk.len();
    }
    ranges
}

/// What the toolkit's registry calls the language of this path, if it knows
/// one. The name, not a grammar: the registry owns those.
pub fn language_for(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    // A few files are named, not suffixed.
    let by_name = match name {
        "Makefile" | "makefile" | "GNUmakefile" | "Makefile.am" | "Makefile.in" => Some("make"),
        "CMakeLists.txt" => Some("cmake"),
        "Cargo.lock" | "uv.lock" | "poetry.lock" => Some("toml"),
        "Gemfile" | "Rakefile" | "Guardfile" | "Podfile" | "Brewfile" => Some("ruby"),
        ".bashrc" | ".bash_profile" | ".zshrc" | ".profile" => Some("bash"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let extension = name.rsplit_once('.').map(|(_, extension)| extension)?;
    Some(match extension.to_lowercase().as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "py" | "pyi" | "pyw" => "python",
        "go" => "go",
        "rb" | "rake" | "gemspec" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "css" | "scss" => "css",
        "html" | "htm" | "vue" => "html",
        "json" | "jsonc" | "json5" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" | "mdx" => "markdown",
        "sh" | "bash" | "zsh" | "ksh" => "bash",
        "lua" => "lua",
        "php" | "phtml" => "php",
        "proto" => "proto",
        "sql" => "sql",
        "scala" | "sbt" => "scala",
        "svelte" => "svelte",
        "astro" => "astro",
        "ejs" => "ejs",
        "erb" => "erb",
        "graphql" | "gql" => "graphql",
        "ex" | "exs" => "elixir",
        "zig" => "zig",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_read_for_its_language() {
        assert_eq!(language_for("crates/e1-ui/src/code.rs"), Some("rust"));
        assert_eq!(language_for("app/page.TSX"), Some("tsx"));
        assert_eq!(language_for("Makefile"), Some("make"));
        assert_eq!(language_for("deep/in/here/Cargo.lock"), Some("toml"));
        assert_eq!(language_for("README.md"), Some("markdown"));
    }

    #[test]
    fn a_path_with_no_grammar_is_left_alone() {
        assert_eq!(language_for("LICENSE"), None);
        assert_eq!(language_for(".gitignore"), None);
        assert_eq!(language_for("assets/logo.png"), None);
    }

    #[test]
    fn lines_are_split_the_way_the_view_splits_them() {
        let text = "one\r\ntwo\nthree\n";
        let ranges = line_ranges(text);
        let taken: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(taken, text.lines().collect::<Vec<_>>());
        assert_eq!(line_ranges("").len(), 0);
        assert_eq!(line_ranges("no newline").len(), 1);
    }

    #[test]
    fn a_file_is_parsed_and_its_lines_carry_styles() {
        let code = Code::parse("main.rs", "fn main() {\n    let x = 1;\n}\n").expect("rust");
        assert_eq!(code.language(), "rust");
        let theme = HighlightTheme::default_dark();
        // `fn` is a keyword, so the first line is not one flat run.
        assert!(code.line(0, &theme).len() > 1);
        // Past the end is empty rather than a panic.
        assert!(code.line(99, &theme).is_empty());
    }

    #[test]
    fn a_huge_file_is_left_plain() {
        let text = "x\n".repeat(MAX_BYTES);
        assert!(Code::parse("big.rs", &text).is_none());
    }
}
