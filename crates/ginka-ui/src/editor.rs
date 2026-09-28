//! File-editor state that can be decided without constructing a GPUI view.

use ginka_protocol::model::FileContent;
use std::ops::Range;
use std::path::Path;

/// A language-server definition that can be opened inside the current worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionTarget {
    /// Slash-separated path relative to the worktree root.
    pub path: String,
    /// UTF-16 range reported by the language server.
    pub range: lsp_types::Range,
}

/// Resolve a language-server file URI to a safe path inside canonical `worktree`.
///
/// Definitions in dependencies or virtual documents deliberately stay with the
/// language-server integration instead of escaping the workspace file API.
pub fn definition_target(
    worktree: &Path,
    uri: &str,
    range: lsp_types::Range,
) -> Option<DefinitionTarget> {
    let absolute = url::Url::parse(uri).ok()?.to_file_path().ok()?;
    let relative = absolute.strip_prefix(worktree).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    let path = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some(DefinitionTarget { path, range })
}

/// Convert one UTF-16 language-server range to UTF-8 editor byte offsets.
///
/// Invalid positions are clamped to the nearest character boundary so a stale
/// server answer cannot create an invalid editor selection.
pub fn definition_selection(text: &str, range: lsp_types::Range) -> Range<usize> {
    let start = lsp_position_offset(text, range.start);
    let end = lsp_position_offset(text, range.end);
    start.min(end)..start.max(end)
}

fn lsp_position_offset(text: &str, position: lsp_types::Position) -> usize {
    let Some(line) = text.split('\n').nth(position.line as usize) else {
        return text.len();
    };
    let line_start = text
        .split('\n')
        .take(position.line as usize)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let mut utf16 = 0_u32;
    for (byte, character) in line.char_indices() {
        if utf16 >= position.character {
            return line_start + byte;
        }
        let next = utf16 + character.len_utf16() as u32;
        if next > position.character {
            return line_start + byte;
        }
        utf16 = next;
    }
    line_start + line.len()
}

/// Whether the file surface can save its current editor contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveState {
    /// Binary and oversized previews are intentionally read-only.
    ReadOnly,
    /// The editor still matches the revision read from the daemon.
    Clean,
    /// The editor differs and can be saved against its original revision.
    Dirty,
}

/// A rich presentation available for a complete workspace file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewKind {
    /// Render the current editor buffer as Markdown.
    Markdown,
    /// Show daemon-recognized image bytes without reading a remote URL.
    Image,
}

/// The ordered file tabs in one workspace's editor surface.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileTabs {
    paths: Vec<String>,
    active: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
}

/// Decide how the save control behaves for a loaded file.
pub fn save_state(file: &FileContent, editor_text: &str) -> SaveState {
    if file.binary || file.truncated {
        SaveState::ReadOnly
    } else if file.text == editor_text {
        SaveState::Clean
    } else {
        SaveState::Dirty
    }
}

/// Pick the rich presentation supported by a complete file.
pub fn preview_kind(file: &FileContent) -> Option<PreviewKind> {
    if file.truncated {
        return None;
    }
    if file.image.is_some() {
        return Some(PreviewKind::Image);
    }
    if file.binary {
        return None;
    }
    let extension = Path::new(&file.path).extension()?.to_str()?;
    ["md", "markdown", "mdx"]
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        .then_some(PreviewKind::Markdown)
}

/// Build a local data URL only for image media types the daemon recognizes.
pub fn image_data_url(file: &FileContent) -> Option<String> {
    if preview_kind(file) != Some(PreviewKind::Image) {
        return None;
    }
    let image = file.image.as_ref()?;
    ["image/png", "image/jpeg", "image/gif", "image/webp"]
        .contains(&image.media_type.as_str())
        .then(|| format!("data:{};base64,{}", image.media_type, image.data_base64))
}

/// Return safe live editor text when Markdown preview was requested.
///
/// Image markers are escaped so rendering a repository document never fetches
/// a remote resource. Image files use their own controlled preview path.
pub fn markdown_preview(file: &FileContent, editor_text: &str, requested: bool) -> Option<String> {
    (requested && preview_kind(file) == Some(PreviewKind::Markdown))
        .then(|| editor_text.replace("![", "\\!["))
}

impl FileTabs {
    /// The paths in strip order.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// The path currently in front.
    pub fn active(&self) -> Option<&str> {
        self.active
            .and_then(|index| self.paths.get(index))
            .map(String::as_str)
    }

    /// Open a path at the end, or bring its existing tab to the front.
    pub fn open(&mut self, path: impl Into<String>) {
        let path = path.into();
        if let Some(index) = self.paths.iter().position(|open| open == &path) {
            self.active = Some(index);
            self.record_visit(path);
            return;
        }
        self.paths.push(path.clone());
        self.active = Some(self.paths.len() - 1);
        self.record_visit(path);
    }

    /// Bring an existing tab to the front.
    pub fn focus(&mut self, path: &str) {
        if let Some(index) = self.paths.iter().position(|open| open == path) {
            self.active = Some(index);
            self.record_visit(path.to_string());
        }
    }

    /// Close a tab and choose the nearest remaining neighbour.
    ///
    /// Dirty buffers are refused because closing a view must not discard work.
    pub fn close(&mut self, path: &str, state: SaveState) -> bool {
        if state == SaveState::Dirty {
            return false;
        }
        let Some(index) = self.paths.iter().position(|open| open == path) else {
            return false;
        };
        let active = self.active;
        self.paths.remove(index);
        self.active = match (self.paths.is_empty(), active) {
            (true, _) => None,
            (false, Some(active)) if index < active => Some(active - 1),
            (false, Some(active)) if index == active => {
                Some(index.saturating_sub(1).min(self.paths.len() - 1))
            }
            (false, active) => active,
        };
        true
    }

    /// Forget every tab when its workspace leaves the screen.
    pub fn clear(&mut self) {
        self.paths.clear();
        self.active = None;
        self.history.clear();
        self.history_index = None;
    }

    /// Whether an earlier visited file can be shown.
    pub fn can_go_back(&self) -> bool {
        self.history_index.is_some_and(|index| index > 0)
    }

    /// Whether a later visited file can be shown.
    pub fn can_go_forward(&self) -> bool {
        self.history_index
            .is_some_and(|index| index + 1 < self.history.len())
    }

    /// Move to the preceding visit, returning a path that may need reloading.
    pub fn go_back(&mut self) -> Option<String> {
        let index = self.history_index?.checked_sub(1)?;
        self.history_index = Some(index);
        let path = self.history[index].clone();
        self.activate_if_open(&path);
        Some(path)
    }

    /// Move to the following visit, returning a path that may need reloading.
    pub fn go_forward(&mut self) -> Option<String> {
        let index = self.history_index? + 1;
        let path = self.history.get(index)?.clone();
        self.history_index = Some(index);
        self.activate_if_open(&path);
        Some(path)
    }

    /// Show a history target after it has been reloaded, without recording a
    /// new visit and destroying the forward path.
    pub fn restore(&mut self, path: impl Into<String>) {
        let path = path.into();
        if let Some(index) = self.paths.iter().position(|open| open == &path) {
            self.active = Some(index);
        } else {
            self.paths.push(path);
            self.active = Some(self.paths.len() - 1);
        }
    }

    fn activate_if_open(&mut self, path: &str) {
        if let Some(index) = self.paths.iter().position(|open| open == path) {
            self.active = Some(index);
        }
    }

    fn record_visit(&mut self, path: String) {
        if self.history_index.and_then(|index| self.history.get(index)) == Some(&path) {
            return;
        }
        if let Some(index) = self.history_index {
            self.history.truncate(index + 1);
        }
        self.history.push(path);
        self.history_index = Some(self.history.len() - 1);
    }
}

/// Pick the tree-sitter language name understood by `gpui-component`.
pub fn language_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rs") => "rust",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("md" | "mdx") => "markdown",
        Some("py") => "python",
        Some("rb") => "ruby",
        Some("go") => "go",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("yml" | "yaml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("json") => "json",
        Some("toml") => "toml",
        _ => "text",
    }
}

/// Format a non-empty editor selection as a one-based file-line reference.
///
/// `selection` uses UTF-8 byte offsets, matching `EditorState`. An exclusive
/// end at the next line's column zero belongs to the preceding selected line.
pub fn selection_reference(path: &str, text: &str, selection: Range<usize>) -> Option<String> {
    let start = floor_char_boundary(text, selection.start.min(text.len()));
    let end = floor_char_boundary(text, selection.end.min(text.len()));
    if start >= end {
        return None;
    }

    let start_line = line_at(text, start);
    let last_character = text[..end].char_indices().next_back()?.0;
    let end_line = line_at(text, last_character);
    let file = if path.chars().any(char::is_whitespace) {
        format!("`{path}`")
    } else {
        format!("@{path}")
    };
    let location = if start_line == end_line {
        format!("line {start_line}")
    } else {
        format!("lines {start_line}-{end_line}")
    };
    Some(format!("{file} ({location})"))
}

/// Copy a non-empty editor selection at valid UTF-8 boundaries.
///
/// Unlike a source reference this may come from a dirty buffer: it is pasted
/// as literal terminal input rather than presented as worktree context.
pub fn selected_text(text: &str, selection: Range<usize>) -> Option<String> {
    let start = floor_char_boundary(text, selection.start.min(text.len()));
    let end = floor_char_boundary(text, selection.end.min(text.len()));
    (start < end).then(|| text[start..end].to_string())
}

/// Reference a selection only while the editor still matches the disk read.
///
/// A dirty buffer's line numbers describe text the agent cannot read from the
/// worktree yet, so presenting them as source context would be misleading.
pub fn saved_selection_reference(
    file: &FileContent,
    editor_text: &str,
    selection: Range<usize>,
) -> Option<String> {
    (save_state(file, editor_text) == SaveState::Clean)
        .then(|| selection_reference(&file.path, editor_text, selection))
        .flatten()
}

/// Add a source reference to a draft as its own paragraph.
pub fn append_reference(draft: &str, reference: &str) -> String {
    if draft.is_empty() {
        return reference.to_string();
    }
    let separator = if draft.ends_with("\n\n") {
        ""
    } else if draft.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{draft}{separator}{reference}")
}

/// Nearest UTF-8 boundary at or before a byte offset.
fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// One-based line containing `offset`, which must be a character boundary.
fn line_at(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1
}

/// One-based line at an editor byte offset, clamped to the live buffer.
pub fn cursor_line(text: &str, offset: usize) -> u32 {
    line_at(text, floor_char_boundary(text, offset.min(text.len()))) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_line_counts_unicode_and_clamps_stale_offsets() {
        let text = "あ\nsecond\n";
        assert_eq!(cursor_line(text, 0), 1);
        assert_eq!(cursor_line(text, 2), 1);
        assert_eq!(cursor_line(text, 4), 2);
        assert_eq!(cursor_line(text, usize::MAX), 3);
    }

    fn file(text: &str) -> FileContent {
        FileContent {
            path: "src/main.rs".into(),
            text: text.into(),
            revision: "revision-1".into(),
            binary: false,
            truncated: false,
            image: None,
        }
    }

    #[test]
    fn only_a_changed_complete_text_file_is_dirty() {
        let clean = file("before");
        assert_eq!(save_state(&clean, "before"), SaveState::Clean);
        assert_eq!(save_state(&clean, "after"), SaveState::Dirty);

        let mut binary = clean.clone();
        binary.binary = true;
        assert_eq!(save_state(&binary, "after"), SaveState::ReadOnly);

        let mut truncated = clean;
        truncated.truncated = true;
        assert_eq!(save_state(&truncated, "after"), SaveState::ReadOnly);
    }

    #[test]
    fn common_source_paths_select_an_editor_language() {
        assert_eq!(language_for_path("src/main.rs"), "rust");
        assert_eq!(language_for_path("web/panel.tsx"), "tsx");
        assert_eq!(language_for_path("README.md"), "markdown");
        assert_eq!(language_for_path("LICENSE"), "text");
    }

    #[test]
    fn markdown_files_offer_preview_case_insensitively() {
        for path in ["README.md", "guide.markdown", "page.mdx", "NOTES.MD"] {
            let mut markdown = file("# Heading");
            markdown.path = path.into();
            assert_eq!(preview_kind(&markdown), Some(PreviewKind::Markdown));
        }

        let mut rust = file("fn main() {}");
        rust.path = "src/main.rs".into();
        assert_eq!(preview_kind(&rust), None);
    }

    #[test]
    fn incomplete_files_do_not_offer_a_misleading_markdown_preview() {
        let mut incomplete = file("# Heading");
        incomplete.path = "README.md".into();
        incomplete.truncated = true;
        assert_eq!(preview_kind(&incomplete), None);

        incomplete.truncated = false;
        incomplete.binary = true;
        assert_eq!(preview_kind(&incomplete), None);
    }

    #[test]
    fn only_a_complete_daemon_recognized_image_offers_an_image_preview() {
        let mut image = file("");
        image.path = "art/cover.bin".into();
        image.binary = true;
        image.image = Some(ginka_protocol::model::FileImage {
            media_type: "image/png".into(),
            data_base64: "iVBORw0KGgo=".into(),
        });

        assert_eq!(preview_kind(&image), Some(PreviewKind::Image));
        assert_eq!(
            image_data_url(&image).as_deref(),
            Some("data:image/png;base64,iVBORw0KGgo=")
        );

        image.truncated = true;
        assert_eq!(preview_kind(&image), None);
        assert_eq!(image_data_url(&image), None);
    }

    #[test]
    fn markdown_preview_uses_the_live_unsaved_editor_buffer() {
        let mut markdown = file("# Saved");
        markdown.path = "README.md".into();

        assert_eq!(markdown_preview(&markdown, "# Unsaved", false), None);
        assert_eq!(
            markdown_preview(&markdown, "# Unsaved", true),
            Some("# Unsaved".into())
        );
    }

    #[test]
    fn markdown_preview_does_not_load_images_named_by_repository_text() {
        let mut markdown = file("");
        markdown.path = "README.md".into();

        assert_eq!(
            markdown_preview(
                &markdown,
                "before ![tracking](https://example.com/pixel.png) after",
                true,
            )
            .as_deref(),
            Some("before \\![tracking](https://example.com/pixel.png) after")
        );
    }

    #[test]
    fn a_selection_becomes_an_exact_one_based_file_line_reference() {
        let text = "first\nsecond\nthird\n";
        assert_eq!(
            selection_reference("src/main.rs", text, 6..18).as_deref(),
            Some("@src/main.rs (lines 2-3)")
        );
        assert_eq!(
            selection_reference("src/main.rs", text, 6..13).as_deref(),
            Some("@src/main.rs (line 2)"),
            "an end at the next line's column zero still names the previous line"
        );
    }

    #[test]
    fn selection_lines_use_byte_offsets_without_counting_unicode_as_lines() {
        let text = "日本語\nlet answer = 42;\n";
        let start = text.find("let").unwrap();
        assert_eq!(
            selection_reference("src/lib.rs", text, start..text.len()).as_deref(),
            Some("@src/lib.rs (line 2)")
        );
    }

    #[test]
    fn an_empty_selection_has_nothing_to_add_and_spaced_paths_are_quoted() {
        assert_eq!(selection_reference("src/main.rs", "text", 2..2), None);
        assert_eq!(
            selection_reference("docs/read me.md", "one\ntwo", 0..3).as_deref(),
            Some("`docs/read me.md` (line 1)")
        );
    }

    #[test]
    fn selected_text_is_utf8_safe_and_does_not_require_a_saved_buffer() {
        let text = "a日本語z";
        assert_eq!(selected_text(text, 2..8).as_deref(), Some("日本"));
        assert_eq!(selected_text(text, 3..3), None);
        assert_eq!(
            selected_text("changed in memory", 0..7).as_deref(),
            Some("changed")
        );
    }

    #[test]
    fn adding_a_selection_keeps_an_existing_draft_separate() {
        assert_eq!(
            append_reference("explain this", "@src/main.rs (line 4)"),
            "explain this\n\n@src/main.rs (line 4)"
        );
        assert_eq!(
            append_reference("", "@src/main.rs (line 4)"),
            "@src/main.rs (line 4)"
        );
    }

    #[test]
    fn an_unsaved_selection_is_not_presented_as_disk_context() {
        let saved = file("first\nsecond\n");
        assert_eq!(
            saved_selection_reference(&saved, "changed\nsecond\n", 0..7),
            None
        );
        assert_eq!(
            saved_selection_reference(&saved, &saved.text, 0..5).as_deref(),
            Some("@src/main.rs (line 1)")
        );
    }

    #[test]
    fn opening_an_existing_file_focuses_one_tab_rather_than_duplicating_it() {
        let mut tabs = FileTabs::default();
        tabs.open("src/main.rs");
        tabs.open("src/lib.rs");
        tabs.open("src/main.rs");

        assert_eq!(tabs.paths(), &["src/main.rs", "src/lib.rs"]);
        assert_eq!(tabs.active(), Some("src/main.rs"));
    }

    #[test]
    fn closing_the_active_file_settles_on_its_left_neighbour() {
        let mut tabs = FileTabs::default();
        for path in ["one.rs", "two.rs", "three.rs"] {
            tabs.open(path);
        }

        tabs.close("three.rs", SaveState::Clean);
        assert_eq!(tabs.active(), Some("two.rs"));
        tabs.close("one.rs", SaveState::Clean);
        assert_eq!(tabs.active(), Some("two.rs"));
        tabs.close("two.rs", SaveState::Clean);
        assert_eq!(tabs.active(), None);
    }

    #[test]
    fn closing_a_background_file_does_not_move_the_active_file() {
        let mut tabs = FileTabs::default();
        for path in ["one.rs", "two.rs", "three.rs"] {
            tabs.open(path);
        }
        tabs.focus("two.rs");
        tabs.close("one.rs", SaveState::Clean);

        assert_eq!(tabs.paths(), &["two.rs", "three.rs"]);
        assert_eq!(tabs.active(), Some("two.rs"));
    }

    #[test]
    fn leaving_a_workspace_clears_its_file_tabs() {
        let mut tabs = FileTabs::default();
        tabs.open("src/main.rs");
        tabs.clear();

        assert!(tabs.paths().is_empty());
        assert_eq!(tabs.active(), None);
    }

    #[test]
    fn a_dirty_tab_is_not_silently_closed() {
        let mut tabs = FileTabs::default();
        tabs.open("src/main.rs");

        assert!(!tabs.close("src/main.rs", SaveState::Dirty));
        assert_eq!(tabs.active(), Some("src/main.rs"));
    }

    #[test]
    fn editor_history_moves_back_and_forward_through_file_visits() {
        let mut tabs = FileTabs::default();
        for path in ["one.rs", "two.rs", "three.rs"] {
            tabs.open(path);
        }

        assert_eq!(tabs.go_back().as_deref(), Some("two.rs"));
        assert_eq!(tabs.active(), Some("two.rs"));
        assert_eq!(tabs.go_back().as_deref(), Some("one.rs"));
        assert!(!tabs.can_go_back());
        assert_eq!(tabs.go_forward().as_deref(), Some("two.rs"));
        assert_eq!(tabs.active(), Some("two.rs"));
    }

    #[test]
    fn a_new_visit_after_going_back_discards_the_forward_branch() {
        let mut tabs = FileTabs::default();
        for path in ["one.rs", "two.rs", "three.rs"] {
            tabs.open(path);
        }
        tabs.go_back();
        tabs.open("four.rs");

        assert!(!tabs.can_go_forward());
        assert_eq!(tabs.go_back().as_deref(), Some("two.rs"));
    }

    #[test]
    fn history_can_restore_a_tab_that_was_closed_without_losing_forward() {
        let mut tabs = FileTabs::default();
        tabs.open("one.rs");
        tabs.open("two.rs");
        tabs.close("one.rs", SaveState::Clean);

        assert_eq!(tabs.go_back().as_deref(), Some("one.rs"));
        assert_eq!(
            tabs.active(),
            Some("two.rs"),
            "the closed path needs loading"
        );
        tabs.restore("one.rs");
        assert_eq!(tabs.active(), Some("one.rs"));
        assert_eq!(tabs.go_forward().as_deref(), Some("two.rs"));
    }

    #[test]
    fn choosing_the_active_tab_twice_adds_no_duplicate_history_step() {
        let mut tabs = FileTabs::default();
        tabs.open("one.rs");
        tabs.open("two.rs");
        tabs.focus("two.rs");
        tabs.focus("two.rs");

        assert_eq!(tabs.go_back().as_deref(), Some("one.rs"));
        assert_eq!(tabs.go_back(), None);
    }

    #[test]
    fn a_file_uri_inside_the_worktree_becomes_a_relative_definition_target() {
        let root = Path::new("/tmp/a workspace");
        let target = definition_target(
            root,
            "file:///tmp/a%20workspace/src/lib.rs",
            lsp_types::Range::new(
                lsp_types::Position::new(4, 2),
                lsp_types::Position::new(4, 8),
            ),
        )
        .expect("workspace target");

        assert_eq!(target.path, "src/lib.rs");
        assert_eq!(target.range.start, lsp_types::Position::new(4, 2));
        assert_eq!(target.range.end, lsp_types::Position::new(4, 8));
    }

    #[test]
    fn definitions_outside_the_worktree_or_without_a_file_uri_are_refused() {
        let root = Path::new("/tmp/worktree");
        let range = lsp_types::Range::default();

        assert_eq!(
            definition_target(root, "file:///tmp/other/src/lib.rs", range),
            None
        );
        assert_eq!(
            definition_target(root, "https://example.com/lib.rs", range),
            None
        );
    }

    #[test]
    fn a_definition_range_counts_utf16_but_selects_utf8_bytes() {
        let text = "zero\n中a🙂b\nlast";
        let range = lsp_types::Range::new(
            lsp_types::Position::new(1, 1),
            lsp_types::Position::new(1, 4),
        );

        assert_eq!(definition_selection(text, range), 8..13);
        assert_eq!(
            &text[definition_selection(text, range)],
            "a🙂",
            "one BMP character and one surrogate pair precede the end"
        );
    }

    #[test]
    fn a_stale_reversed_definition_range_is_normalized() {
        let range = lsp_types::Range::new(
            lsp_types::Position::new(0, 4),
            lsp_types::Position::new(0, 1),
        );

        assert_eq!(definition_selection("abcdef", range), 1..4);
    }
}
