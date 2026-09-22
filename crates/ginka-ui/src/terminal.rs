//! What a terminal's bytes look like on a screen.
//!
//! The daemon owns the shell and forwards what it prints, escapes and all;
//! this is the half that decides what those escapes mean. It is here rather
//! than in the views because a terminal emulator has behaviour worth testing —
//! wrapping, clearing, colour, the cursor — and none of it needs a window.
//!
//! `alacritty_terminal` does the emulation, as the roadmap picked (§4.3). What
//! this adds is the shape a view can draw without knowing anything about
//! alacritty's grid.

// `VoidListener` is alacritty's own do-nothing sink: the events it would
// carry — bell, title changes, clipboard requests — are for a terminal
// application to act on, and this screen only draws.
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use ginka_protocol::TerminalId;
use ginka_protocol::model::TerminalInfo;
use gpui::Keystroke;
use std::ops::Range;
use std::path::{Component, Path};

/// A workspace file location recognized in one terminal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFileLink {
    /// Character columns occupied by the clickable text in the rendered row.
    pub columns: Range<usize>,
    /// Slash-separated path relative to the workspace root.
    pub path: String,
    /// One-based line number, when the terminal printed one.
    pub line: Option<u32>,
    /// One-based column number, when the terminal printed one.
    pub column: Option<u32>,
}

impl TerminalFileLink {
    /// Convert the printed one-based position into an editor selection.
    pub fn selection_range(&self) -> Option<lsp_types::Range> {
        let line = self.line?.saturating_sub(1);
        let character = self.column.unwrap_or(1).saturating_sub(1);
        Some(lsp_types::Range::new(
            lsp_types::Position::new(line, character),
            lsp_types::Position::new(line, character.saturating_add(1)),
        ))
    }
}

/// Find safe workspace file locations in a row of terminal text.
///
/// Absolute paths are accepted only beneath `worktree`; relative paths may
/// not contain a parent component. The conservative filename check avoids
/// turning ordinary shell words and URLs into misleading links.
pub fn file_links(line: &str, worktree: &Path) -> Vec<TerminalFileLink> {
    let characters = line.chars().collect::<Vec<_>>();
    let mut links = Vec::new();
    let mut cursor = 0;
    while cursor < characters.len() {
        while cursor < characters.len() && characters[cursor].is_whitespace() {
            cursor += 1;
        }
        let token_start = cursor;
        while cursor < characters.len() && !characters[cursor].is_whitespace() {
            cursor += 1;
        }
        let token_end = cursor;
        if token_start == token_end {
            continue;
        }

        let mut start = token_start;
        let mut end = token_end;
        while start < end && matches!(characters[start], '(' | '[' | '{' | '<' | '"' | '\'' | '`') {
            start += 1;
        }
        while start < end
            && matches!(
                characters[end - 1],
                ')' | ']' | '}' | '>' | '"' | '\'' | '`' | ',' | ';' | '.'
            )
        {
            end -= 1;
        }
        if start == end {
            continue;
        }

        let candidate = characters[start..end].iter().collect::<String>();
        if let Some((path, line, column)) = parse_file_location(&candidate, worktree) {
            links.push(TerminalFileLink {
                columns: start..end,
                path,
                line,
                column,
            });
        }
    }
    links
}

fn parse_file_location(
    candidate: &str,
    worktree: &Path,
) -> Option<(String, Option<u32>, Option<u32>)> {
    if candidate.contains("://") {
        return None;
    }
    let (without_last, last) = numeric_suffix(candidate);
    let (raw_path, line, column) = match (without_last, last) {
        (Some(without_last), Some(last)) => {
            let (without_line, previous) = numeric_suffix(without_last);
            match (without_line, previous) {
                (Some(path), Some(line)) => (path, Some(line), Some(last)),
                _ => (without_last, Some(last), None),
            }
        }
        _ => (candidate, None, None),
    };
    if line == Some(0) || column == Some(0) {
        return None;
    }

    let raw_path = Path::new(raw_path);
    let relative = if raw_path.is_absolute() {
        raw_path.strip_prefix(worktree).ok()?
    } else {
        raw_path
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    let filename = parts.last()?;
    if parts.len() == 1 && line.is_none() && !standalone_filename(filename) {
        return None;
    }
    Some((parts.join("/"), line, column))
}

fn standalone_filename(filename: &str) -> bool {
    if matches!(
        filename.to_ascii_lowercase().as_str(),
        "makefile" | "dockerfile" | "justfile" | "gemfile" | "rakefile" | "license"
    ) {
        return true;
    }
    let Some((_, extension)) = filename.rsplit_once('.') else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "rs" | "toml"
            | "md"
            | "json"
            | "yml"
            | "yaml"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "css"
            | "scss"
            | "html"
            | "htm"
            | "sh"
            | "zsh"
            | "bash"
            | "fish"
            | "sql"
            | "rb"
            | "php"
            | "swift"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "vue"
            | "svelte"
            | "lock"
            | "txt"
            | "xml"
            | "proto"
    )
}

fn numeric_suffix(value: &str) -> (Option<&str>, Option<u32>) {
    let Some((before, suffix)) = value.rsplit_once(':') else {
        return (None, None);
    };
    match suffix.parse::<u32>() {
        Ok(number) => (Some(before), Some(number)),
        Err(_) => (None, None),
    }
}

/// One character on the screen, with how it should be drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenCell {
    pub text: char,
    /// `None` means the theme's ordinary text colour.
    pub foreground: Option<TerminalColor>,
    /// `None` means the terminal's own background.
    pub background: Option<TerminalColor>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Whether the cursor is sitting on this cell.
    pub cursor: bool,
    /// Whether this cell belongs to the selected terminal-search result.
    pub search_match: bool,
}

/// A colour a terminal asked for.
///
/// The sixteen named ones are left named rather than resolved to hex here: the
/// theme decides what "red" looks like on its own background, and a terminal
/// that hardcoded them would clash with every theme but the one it was written
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    Named(u8),
    Rgb(u8, u8, u8),
}

/// One row of the screen.
pub type ScreenRow = Vec<ScreenCell>;

/// One literal match in the terminal's live grid or bounded history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSearchMatch {
    line: i32,
    columns: Range<usize>,
}

/// A terminal's screen, fed by the bytes its shell prints.
pub struct TerminalScreen {
    term: Term<VoidListener>,
    parser: Processor,
    rows: u16,
    cols: u16,
}

/// The size a terminal was told it has.
#[derive(Debug, Clone, Copy)]
struct Size {
    rows: usize,
    cols: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

impl TerminalScreen {
    /// An empty screen of `rows` by `cols`.
    pub fn new(rows: u16, cols: u16) -> Self {
        let size = Size {
            rows: rows.max(1) as usize,
            cols: cols.max(1) as usize,
        };
        Self {
            term: Term::new(Config::default(), &size, VoidListener),
            parser: Processor::new(),
            rows: rows.max(1),
            cols: cols.max(1),
        }
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Number of history lines above the live viewport currently displayed.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Move through bounded terminal history; positive lines move backward.
    pub fn scroll(&mut self, lines: i32) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(lines));
        }
    }

    /// Return directly to the live output at the bottom of history.
    pub fn scroll_to_live(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Find every line-local literal match from oldest output to newest.
    ///
    /// Lowercase queries ignore ASCII case; once the query contains an
    /// uppercase character it is exact. This mirrors the terminal emulator's
    /// smart-case convention without treating user input as a regular
    /// expression.
    pub fn search(&self, query: &str) -> Vec<TerminalSearchMatch> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let query = query.chars().collect::<Vec<_>>();
        let exact = query.iter().any(|character| character.is_uppercase());
        let grid = self.term.grid();
        let mut matches = Vec::new();
        for line in grid.topmost_line().0..=grid.bottommost_line().0 {
            let row = (0..self.cols as usize)
                .map(|column| grid[Point::new(Line(line), Column(column))].c)
                .collect::<Vec<_>>();
            if row.len() < query.len() {
                continue;
            }
            for start in 0..=row.len() - query.len() {
                let candidate = &row[start..start + query.len()];
                let found = candidate.iter().zip(&query).all(|(left, right)| {
                    if exact {
                        left == right
                    } else {
                        left.eq_ignore_ascii_case(right)
                    }
                });
                if found {
                    matches.push(TerminalSearchMatch {
                        line,
                        columns: start..start + query.len(),
                    });
                }
            }
        }
        matches
    }

    /// Encode clipboard text for the terminal's current input mode.
    pub fn paste_input(&self, text: &str) -> String {
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
        } else {
            text.replace("\r\n", "\n").replace('\n', "\r")
        }
    }

    /// Translate a platform keystroke into bytes understood by this terminal.
    pub fn key_input(&self, keystroke: &Keystroke) -> Option<String> {
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;

        // Platform modifiers belong to the window and its command bindings;
        // they must never leak their printable key into a shell.
        if modifiers.platform {
            return None;
        }

        if key == "enter" && modifiers.shift && !modifiers.control && !modifiers.alt {
            return Some("\n".into());
        }
        if key == "backspace" && modifiers.control && !modifiers.shift && !modifiers.alt {
            return Some("\x08".into());
        }

        let control = modifiers.control.then(|| match key {
            "space" | "@" => Some('\0'),
            "[" => Some('\x1b'),
            "\\" => Some('\x1c'),
            "]" => Some('\x1d'),
            "^" => Some('\x1e'),
            "_" => Some('\x1f'),
            "?" => Some('\x7f'),
            _ if key.len() == 1 => key
                .chars()
                .next()
                .map(|letter| letter.to_ascii_lowercase())
                .filter(char::is_ascii_lowercase)
                .map(|letter| (letter as u8 - b'a' + 1) as char),
            _ => None,
        });

        if control.as_ref().is_none_or(Option::is_none) {
            let modifier = 1
                + u8::from(modifiers.shift)
                + 2 * u8::from(modifiers.alt)
                + 4 * u8::from(modifiers.control);
            let modified = match key {
                "up" => Some(format!("\x1b[1;{modifier}A")),
                "down" => Some(format!("\x1b[1;{modifier}B")),
                "right" => Some(format!("\x1b[1;{modifier}C")),
                "left" => Some(format!("\x1b[1;{modifier}D")),
                "home" => Some(format!("\x1b[1;{modifier}H")),
                "end" => Some(format!("\x1b[1;{modifier}F")),
                "insert" => Some(format!("\x1b[2;{modifier}~")),
                "delete" => Some(format!("\x1b[3;{modifier}~")),
                "pageup" => Some(format!("\x1b[5;{modifier}~")),
                "pagedown" => Some(format!("\x1b[6;{modifier}~")),
                "f1" => Some(format!("\x1b[1;{modifier}P")),
                "f2" => Some(format!("\x1b[1;{modifier}Q")),
                "f3" => Some(format!("\x1b[1;{modifier}R")),
                "f4" => Some(format!("\x1b[1;{modifier}S")),
                "f5" => Some(format!("\x1b[15;{modifier}~")),
                "f6" => Some(format!("\x1b[17;{modifier}~")),
                "f7" => Some(format!("\x1b[18;{modifier}~")),
                "f8" => Some(format!("\x1b[19;{modifier}~")),
                "f9" => Some(format!("\x1b[20;{modifier}~")),
                "f10" => Some(format!("\x1b[21;{modifier}~")),
                "f11" => Some(format!("\x1b[23;{modifier}~")),
                "f12" => Some(format!("\x1b[24;{modifier}~")),
                _ => None,
            };
            if modifier > 1
                && let Some(modified) = modified
            {
                return Some(modified);
            }
        }

        let application_cursor = self.term.mode().contains(TermMode::APP_CURSOR);
        let named = match (key, modifiers.shift) {
            ("tab", true) => Some("\x1b[Z"),
            ("enter", _) => Some("\r"),
            ("tab", _) => Some("\t"),
            ("backspace", _) => Some("\x7f"),
            ("escape", _) => Some("\x1b"),
            ("up", _) if application_cursor => Some("\x1bOA"),
            ("down", _) if application_cursor => Some("\x1bOB"),
            ("right", _) if application_cursor => Some("\x1bOC"),
            ("left", _) if application_cursor => Some("\x1bOD"),
            ("home", _) if application_cursor => Some("\x1bOH"),
            ("end", _) if application_cursor => Some("\x1bOF"),
            ("up", _) => Some("\x1b[A"),
            ("down", _) => Some("\x1b[B"),
            ("right", _) => Some("\x1b[C"),
            ("left", _) => Some("\x1b[D"),
            ("home", _) => Some("\x1b[H"),
            ("end", _) => Some("\x1b[F"),
            ("insert", _) => Some("\x1b[2~"),
            ("delete", _) => Some("\x1b[3~"),
            ("pageup", _) => Some("\x1b[5~"),
            ("pagedown", _) => Some("\x1b[6~"),
            ("f1", _) => Some("\x1bOP"),
            ("f2", _) => Some("\x1bOQ"),
            ("f3", _) => Some("\x1bOR"),
            ("f4", _) => Some("\x1bOS"),
            ("f5", _) => Some("\x1b[15~"),
            ("f6", _) => Some("\x1b[17~"),
            ("f7", _) => Some("\x1b[18~"),
            ("f8", _) => Some("\x1b[19~"),
            ("f9", _) => Some("\x1b[20~"),
            ("f10", _) => Some("\x1b[21~"),
            ("f11", _) => Some("\x1b[23~"),
            ("f12", _) => Some("\x1b[24~"),
            ("space", _) => Some(" "),
            _ => None,
        };

        let mut input = control
            .flatten()
            .map(|character| character.to_string())
            .or_else(|| named.map(str::to_string))
            .or_else(|| keystroke.key_char.clone().filter(|typed| !typed.is_empty()))?;
        if modifiers.alt {
            input.insert(0, '\x1b');
        }
        Some(input)
    }

    /// Move the viewport far enough for a search match to be visible.
    pub fn reveal_search_match(&mut self, found: &TerminalSearchMatch) {
        let target = found.line.saturating_neg().max(0) as usize;
        let current = self.display_offset();
        self.term
            .scroll_display(Scroll::Delta(target as i32 - current as i32));
    }

    /// Feed it what the shell printed.
    pub fn feed(&mut self, data: &str) {
        self.parser.advance(&mut self.term, data.as_bytes());
    }

    /// Tell it the window is a different size now.
    ///
    /// The daemon has to be told separately — it owns the pty, and the shell
    /// learns its size from there — but the screen has to agree or the text
    /// wraps at a column the shell is not using.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if (rows, cols) == (self.rows, self.cols) {
            return;
        }
        self.term.resize(Size {
            rows: rows as usize,
            cols: cols as usize,
        });
        self.rows = rows;
        self.cols = cols;
    }

    /// The screen as rows of cells, top to bottom.
    ///
    /// Trailing blanks are kept: a view that trimmed them would have to guess
    /// where a background colour ends, and a shell that painted a bar across
    /// the width would lose its right-hand end.
    pub fn rows_of_cells(&self) -> Vec<ScreenRow> {
        self.rows_of_cells_with_match(None)
    }

    /// The visible rows, marking the selected terminal-search match.
    pub fn rows_of_cells_with_match(
        &self,
        selected: Option<&TerminalSearchMatch>,
    ) -> Vec<ScreenRow> {
        let cursor = self.term.grid().cursor.point;
        let display_offset = self.term.grid().display_offset();
        let mut screen = Vec::with_capacity(self.rows as usize);

        for line in 0..self.rows as usize {
            let mut row = Vec::with_capacity(self.cols as usize);
            for column in 0..self.cols as usize {
                let point =
                    viewport_to_point(display_offset, Point::<usize>::new(line, Column(column)));
                let cell = &self.term.grid()[point];
                row.push(ScreenCell {
                    text: cell.c,
                    foreground: colour(cell.fg),
                    background: colour(cell.bg),
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                    // Only while looking at the live screen: a cursor drawn
                    // over scrollback is a cursor in the wrong place.
                    cursor: display_offset == 0
                        && cursor.line.0 == line as i32
                        && cursor.column.0 == column,
                    search_match: selected.is_some_and(|found| {
                        found.line == point.line.0 && found.columns.contains(&column)
                    }),
                });
            }
            screen.push(row);
        }
        screen
    }

    /// The screen as plain text, for tests and for "copy all".
    pub fn text(&self) -> String {
        self.rows_of_cells()
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }
}

/// What a cell's colour is, or `None` for the theme's default.
fn colour(from: Color) -> Option<TerminalColor> {
    match from {
        Color::Named(NamedColor::Foreground | NamedColor::Background) => None,
        Color::Named(named) => Some(TerminalColor::Named(named as u8)),
        Color::Spec(rgb) => Some(TerminalColor::Rgb(rgb.r, rgb.g, rgb.b)),
        Color::Indexed(index) => Some(TerminalColor::Named(index)),
    }
}

/// One shell in the dock: what it is called, and the screen it draws on.
pub struct TerminalTab {
    pub id: TerminalId,
    pub title: String,
    pub screen: TerminalScreen,
}

/// What the view should do with one request to close a running terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseRequest {
    /// The terminal is still present and needs one explicit confirmation.
    Confirm,
    /// The second request removed the tab and should reach the daemon.
    Close,
    /// The terminal already disappeared, so there is nothing to stop.
    Missing,
}

/// The shells the dock is showing, and which one is in front.
///
/// The daemon owns the ptys; this is only the strip and the screens. A window
/// that reopens finds the shells still running and adopts them, which is why
/// `adopt` exists at all: the tab a user left is not one this window created.
#[derive(Default)]
pub struct TerminalTabs {
    tabs: Vec<TerminalTab>,
    active: usize,
    armed_close: Option<TerminalId>,
    split: Option<[TerminalId; 2]>,
}

impl TerminalTabs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every tab, in the order they were opened.
    pub fn tabs(&self) -> &[TerminalTab] {
        &self.tabs
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Every tab, to be drawn on or resized.
    pub fn tabs_mut(&mut self) -> &mut [TerminalTab] {
        &mut self.tabs
    }

    /// Which tab is in front.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The tab in front, if there is one.
    pub fn active(&self) -> Option<&TerminalTab> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs.get_mut(self.active)
    }

    /// The id of the tab in front: where a keystroke goes.
    pub fn active_id(&self) -> Option<TerminalId> {
        self.active().map(|tab| tab.id.clone())
    }

    /// The two terminal ids shown side by side, from left to right.
    pub fn split_ids(&self) -> Option<[TerminalId; 2]> {
        self.split.clone()
    }

    /// Add a shell and bring it to the front, because opening one is asking
    /// to type in it.
    pub fn open(&mut self, id: TerminalId, title: String, rows: u16, cols: u16) {
        self.armed_close = None;
        self.split = None;
        self.tabs.push(TerminalTab {
            id,
            title,
            screen: TerminalScreen::new(rows, cols),
        });
        self.active = self.tabs.len() - 1;
    }

    /// Add a shell to the right of the active terminal and focus it.
    pub fn open_split(&mut self, id: TerminalId, title: String, rows: u16, cols: u16) {
        let Some(left) = self.active_id() else {
            self.open(id, title, rows, cols);
            return;
        };
        self.armed_close = None;
        self.tabs.push(TerminalTab {
            id: id.clone(),
            title,
            screen: TerminalScreen::new(rows, cols),
        });
        self.active = self.tabs.len() - 1;
        self.split = Some([left, id]);
    }

    /// Stop showing two panes without stopping either daemon terminal.
    pub fn unsplit(&mut self) {
        self.split = None;
    }

    /// Restore a persisted split only when both daemon terminals were adopted.
    pub fn restore_split(&mut self, split: [TerminalId; 2], active: Option<&TerminalId>) -> bool {
        self.split = None;
        if split[0] == split[1]
            || split
                .iter()
                .any(|id| !self.tabs.iter().any(|tab| &tab.id == id))
        {
            return false;
        }
        let fallback = split[1].clone();
        self.split = Some(split.clone());
        let active = active
            .filter(|id| split.contains(id))
            .cloned()
            .unwrap_or(fallback);
        self.focus_id(&active);
        true
    }

    /// Show a tab that is already open.
    ///
    /// While split, a hidden tab replaces the focused pane instead of
    /// collapsing the other pane. The tab strip is shared by both panes, so
    /// the focused pane is the only unambiguous destination for that choice.
    pub fn focus(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.armed_close = None;
            let previous = self.active_id();
            self.active = index;
            let id = self.tabs[index].id.clone();
            if self
                .split
                .as_ref()
                .is_some_and(|split| !split.contains(&id))
            {
                let pane = self.split.as_ref().and_then(|split| {
                    previous
                        .as_ref()
                        .and_then(|previous| split.iter().position(|id| id == previous))
                });
                if let Some(pane) = pane {
                    self.split.as_mut().expect("the split was read above")[pane] = id;
                } else {
                    self.split = None;
                }
            }
        }
    }

    /// Focus a shell by id, retaining a split when it is one of its panes.
    pub fn focus_id(&mut self, id: &TerminalId) {
        if let Some(index) = self.tabs.iter().position(|tab| &tab.id == id) {
            self.focus(index);
        }
    }

    /// Focus the next tab, wrapping to the first, and report whether it moved.
    pub fn focus_next(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        self.focus((self.active + 1) % self.tabs.len());
        true
    }

    /// Focus the previous tab, wrapping to the last, and report whether it moved.
    pub fn focus_previous(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        self.focus(if self.active == 0 {
            self.tabs.len() - 1
        } else {
            self.active - 1
        });
        true
    }

    /// Focus the other visible split pane and report whether a split existed.
    pub fn focus_other_pane(&mut self) -> bool {
        let Some(split) = self.split.clone() else {
            return false;
        };
        let Some(active) = self.active_id() else {
            return false;
        };
        let other = if active == split[0] {
            &split[1]
        } else if active == split[1] {
            &split[0]
        } else {
            return false;
        };
        self.focus_id(other);
        true
    }

    /// Whether this terminal is waiting for a second close activation.
    pub fn close_confirmation(&self, id: &TerminalId) -> bool {
        self.armed_close.as_ref() == Some(id)
    }

    /// Ask to stop a running terminal, requiring the same action twice.
    pub fn request_close(&mut self, id: &TerminalId) -> CloseRequest {
        if !self.tabs.iter().any(|tab| &tab.id == id) {
            return CloseRequest::Missing;
        }
        if self.close_confirmation(id) {
            self.close(id);
            CloseRequest::Close
        } else {
            self.armed_close = Some(id.clone());
            CloseRequest::Confirm
        }
    }

    /// Forget a shell, and settle on the one beside it.
    ///
    /// The neighbour to the left, the way every tab strip does it: closing the
    /// third tab leaves the second in front, not the first.
    pub fn close(&mut self, id: &TerminalId) {
        if self.armed_close.as_ref() == Some(id) {
            self.armed_close = None;
        }
        let Some(index) = self.tabs.iter().position(|tab| &tab.id == id) else {
            return;
        };
        if self.split.as_ref().is_some_and(|split| split.contains(id)) {
            self.split = None;
        }
        self.tabs.remove(index);
        self.active = index
            .saturating_sub(1)
            .min(self.tabs.len().saturating_sub(1));
    }

    /// Feed a shell's output to its screen. Output for a tab this window is
    /// not showing is dropped: the daemon keeps the history it can replay.
    pub fn feed(&mut self, id: &TerminalId, data: &str) -> bool {
        match self.tabs.iter_mut().find(|tab| &tab.id == id) {
            Some(tab) => {
                tab.screen.feed(data);
                true
            }
            None => false,
        }
    }

    /// Take on the shells the daemon says are running in this workspace.
    ///
    /// Screens for the ones this window has not seen, and no screen thrown
    /// away for one it has: the tab a user was reading keeps what is on it.
    /// Anything the daemon no longer lists is gone, whoever closed it.
    pub fn adopt(&mut self, running: &[TerminalInfo], rows: u16, cols: u16) -> Vec<TerminalId> {
        let front = self.active_id();
        let mut adopted = Vec::new();
        let mut kept: Vec<TerminalTab> = Vec::new();
        for info in running {
            match self.tabs.iter().position(|tab| tab.id == info.id) {
                Some(index) => {
                    let mut tab = self.tabs.remove(index);
                    tab.title = info.title.clone();
                    kept.push(tab);
                }
                None => {
                    adopted.push(info.id.clone());
                    kept.push(TerminalTab {
                        id: info.id.clone(),
                        title: info.title.clone(),
                        screen: TerminalScreen::new(rows, cols),
                    });
                }
            }
        }
        self.tabs = kept;
        if self
            .armed_close
            .as_ref()
            .is_some_and(|id| !self.tabs.iter().any(|tab| &tab.id == id))
        {
            self.armed_close = None;
        }
        if self.split.as_ref().is_some_and(|split| {
            split
                .iter()
                .any(|id| !self.tabs.iter().any(|tab| &tab.id == id))
        }) {
            self.split = None;
        }
        self.active = front
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or(0);
        adopted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn terminal_locations_keep_the_file_line_and_column() {
        let links = file_links(
            "error at crates/ginka-core/src/git.rs:124:9",
            Path::new("/work/ginka"),
        );

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].path, "crates/ginka-core/src/git.rs");
        assert_eq!(links[0].line, Some(124));
        assert_eq!(links[0].column, Some(9));
        assert_eq!(
            links[0].selection_range(),
            Some(lsp_types::Range::new(
                lsp_types::Position::new(123, 8),
                lsp_types::Position::new(123, 9),
            ))
        );
        assert_eq!(
            &"error at crates/ginka-core/src/git.rs:124:9"
                .chars()
                .collect::<Vec<_>>()[links[0].columns.clone()]
            .iter()
            .collect::<String>(),
            "crates/ginka-core/src/git.rs:124:9"
        );
    }

    #[test]
    fn an_absolute_terminal_location_is_made_workspace_relative() {
        let links = file_links(
            "/work/ginka/src/main.rs:12 failed",
            Path::new("/work/ginka"),
        );

        assert_eq!(links[0].path, "src/main.rs");
        assert_eq!(links[0].line, Some(12));
        assert_eq!(links[0].column, None);
    }

    #[test]
    fn terminal_locations_never_escape_the_workspace() {
        let root = Path::new("/work/ginka");
        assert!(file_links("../secret.txt:1", root).is_empty());
        assert!(file_links("/work/elsewhere/secret.txt:1", root).is_empty());
        assert!(file_links("https://example.test/main.rs:1", root).is_empty());
    }

    #[test]
    fn punctuation_around_a_terminal_location_is_not_part_of_its_path() {
        let links = file_links("failed (./src/main.rs:7:2),", Path::new("/work/ginka"));

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].path, "src/main.rs");
        assert_eq!(links[0].columns, 8..25);
    }

    #[test]
    fn what_the_shell_prints_lands_on_the_screen() {
        let mut screen = TerminalScreen::new(4, 20);
        screen.feed("hello\r\nworld\r\n");
        assert_eq!(screen.text(), "hello\nworld");
    }

    #[test]
    fn a_line_longer_than_the_screen_wraps_rather_than_vanishing() {
        let mut screen = TerminalScreen::new(4, 10);
        screen.feed("0123456789abcde");
        let rows = screen.rows_of_cells();
        assert_eq!(
            rows[0].iter().map(|cell| cell.text).collect::<String>(),
            "0123456789"
        );
        assert!(
            rows[1]
                .iter()
                .map(|cell| cell.text)
                .collect::<String>()
                .starts_with("abcde"),
            "the rest is on the next line"
        );
    }

    #[test]
    fn an_escape_clears_the_screen_rather_than_being_printed() {
        let mut screen = TerminalScreen::new(4, 20);
        screen.feed("noise\r\n");
        screen.feed("\x1b[2J\x1b[H");
        assert_eq!(screen.text(), "", "the escape is an instruction, not text");
    }

    #[test]
    fn colour_survives_as_something_the_theme_can_interpret() {
        let mut screen = TerminalScreen::new(2, 10);
        // Red foreground, then back to normal.
        screen.feed("\x1b[31mred\x1b[0m ok");
        let row = &screen.rows_of_cells()[0];
        assert!(row[0].foreground.is_some(), "the colour was asked for");
        assert_eq!(
            row[4].foreground, None,
            "and the theme's own colour after the reset"
        );
    }

    #[test]
    fn a_truecolour_request_keeps_its_exact_value() {
        let mut screen = TerminalScreen::new(2, 10);
        screen.feed("\x1b[38;2;10;20;30mx");
        assert_eq!(
            screen.rows_of_cells()[0][0].foreground,
            Some(TerminalColor::Rgb(10, 20, 30))
        );
    }

    #[test]
    fn bold_and_underline_are_carried_through() {
        let mut screen = TerminalScreen::new(2, 10);
        screen.feed("\x1b[1mB\x1b[0m\x1b[4mU");
        let row = &screen.rows_of_cells()[0];
        assert!(row[0].bold);
        assert!(!row[0].underline);
        assert!(row[1].underline);
    }

    #[test]
    fn the_cursor_is_where_the_next_character_would_go() {
        let mut screen = TerminalScreen::new(3, 10);
        screen.feed("ab");
        let rows = screen.rows_of_cells();
        assert!(rows[0][2].cursor, "after what has been typed");
        assert!(!rows[0][1].cursor);
    }

    #[test]
    fn a_resize_rewraps_rather_than_keeping_the_old_width() {
        // The shell is told separately, and a screen that disagreed with it
        // would wrap at a column the shell is not using.
        let mut screen = TerminalScreen::new(4, 10);
        screen.feed("0123456789abcde");
        screen.resize(4, 20);
        assert_eq!(screen.cols(), 20);
        assert_eq!(screen.rows_of_cells()[0].len(), 20);
    }

    #[test]
    fn scrollback_can_be_browsed_and_returns_to_the_live_screen() {
        let mut screen = TerminalScreen::new(3, 20);
        screen.feed("one\r\ntwo\r\nthree\r\nfour\r\nfive");
        assert!(!screen.text().contains("one"));

        screen.scroll(10);
        assert!(screen.display_offset() > 0);
        assert!(screen.text().contains("one"));

        screen.scroll_to_live();
        assert_eq!(screen.display_offset(), 0);
        assert!(screen.text().contains("five"));
    }

    #[test]
    fn search_finds_history_with_smart_case_and_reveals_the_match() {
        let mut screen = TerminalScreen::new(3, 20);
        screen.feed("old needle\r\nmiddle\r\nnew NEEDLE\r\ntail");

        let matches = screen.search("needle");
        assert_eq!(matches.len(), 2, "a lowercase query ignores ASCII case");
        assert_eq!(screen.search("NEEDLE").len(), 1, "uppercase is exact");

        screen.reveal_search_match(&matches[0]);
        assert!(screen.display_offset() > 0, "the historical hit is shown");
        let rows = screen.rows_of_cells_with_match(Some(&matches[0]));
        assert_eq!(
            rows.iter()
                .flatten()
                .filter(|cell| cell.search_match)
                .map(|cell| cell.text)
                .collect::<String>(),
            "needle"
        );
    }

    #[test]
    fn an_empty_terminal_search_has_no_matches() {
        let mut screen = TerminalScreen::new(2, 10);
        screen.feed("output");
        assert!(screen.search("").is_empty());
    }

    #[test]
    fn ordinary_terminal_paste_uses_carriage_returns_for_shell_lines() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(screen.paste_input("one\ntwo\r\nthree"), "one\rtwo\rthree");
    }

    #[test]
    fn bracketed_terminal_paste_is_bounded_and_cannot_inject_an_end_marker() {
        let mut screen = TerminalScreen::new(2, 20);
        screen.feed("\x1b[?2004h");

        assert_eq!(
            screen.paste_input("one\ntwo\x1b[201~three"),
            "\x1b[200~one\ntwo[201~three\x1b[201~"
        );
    }

    fn key(source: &str, typed: Option<&str>) -> Keystroke {
        let mut key = Keystroke::parse(source).unwrap();
        key.key_char = typed.map(str::to_string);
        key
    }

    #[test]
    fn terminal_keys_follow_application_cursor_mode() {
        let mut screen = TerminalScreen::new(2, 20);
        assert_eq!(
            screen.key_input(&key("up", None)).as_deref(),
            Some("\x1b[A")
        );

        screen.feed("\x1b[?1h");
        assert_eq!(
            screen.key_input(&key("up", None)).as_deref(),
            Some("\x1bOA")
        );
    }

    #[test]
    fn terminal_keys_include_reverse_tab_and_function_keys() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(
            screen.key_input(&key("shift-tab", None)).as_deref(),
            Some("\x1b[Z")
        );
        assert_eq!(
            screen.key_input(&key("f5", None)).as_deref(),
            Some("\x1b[15~")
        );
    }

    #[test]
    fn terminal_keys_encode_control_symbols_and_alt_text() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(
            screen.key_input(&key("ctrl-space", None)).as_deref(),
            Some("\0")
        );
        assert_eq!(
            screen.key_input(&key("alt-x->ß", Some("ß"))).as_deref(),
            Some("\x1bß")
        );
    }

    #[test]
    fn platform_shortcuts_are_not_typed_into_the_terminal() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(screen.key_input(&key("cmd-c", Some("c"))), None);
    }

    #[test]
    fn modified_terminal_navigation_uses_xterm_modifier_codes() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(
            screen.key_input(&key("ctrl-left", None)).as_deref(),
            Some("\x1b[1;5D")
        );
        assert_eq!(
            screen.key_input(&key("shift-alt-up", None)).as_deref(),
            Some("\x1b[1;4A")
        );
        assert_eq!(
            screen.key_input(&key("ctrl-delete", None)).as_deref(),
            Some("\x1b[3;5~")
        );
    }

    #[test]
    fn modified_terminal_function_keys_keep_their_key_number() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(
            screen.key_input(&key("ctrl-shift-f5", None)).as_deref(),
            Some("\x1b[15;6~")
        );
        assert_eq!(
            screen.key_input(&key("alt-f2", None)).as_deref(),
            Some("\x1b[1;3Q")
        );
    }

    #[test]
    fn shifted_enter_and_control_backspace_reach_cli_programs() {
        let screen = TerminalScreen::new(2, 20);

        assert_eq!(
            screen.key_input(&key("shift-enter", None)).as_deref(),
            Some("\n")
        );
        assert_eq!(
            screen.key_input(&key("ctrl-backspace", None)).as_deref(),
            Some("\x08")
        );
    }

    #[test]
    fn a_screen_keeps_its_trailing_blanks_for_the_view_to_paint() {
        let mut screen = TerminalScreen::new(2, 8);
        screen.feed("ab");
        assert_eq!(
            screen.rows_of_cells()[0].len(),
            8,
            "a shell that painted a bar across the width would lose its end"
        );
    }

    /// What the daemon says about a shell it is running.
    fn info(id: &str, title: &str) -> TerminalInfo {
        TerminalInfo {
            id: TerminalId(id.into()),
            workspace: ginka_protocol::WorkspaceId("project/branch".into()),
            title: title.into(),
        }
    }

    #[test]
    fn opening_a_shell_brings_it_to_the_front() {
        let mut tabs = TerminalTabs::new();
        tabs.open(TerminalId("one".into()), "shell 1".into(), 24, 80);
        tabs.open(TerminalId("two".into()), "shell 2".into(), 24, 80);
        assert_eq!(tabs.active_id(), Some(TerminalId("two".into())));
        assert_eq!(tabs.tabs().len(), 2);
    }

    #[test]
    fn splitting_keeps_two_shells_visible_until_one_is_closed() {
        let mut tabs = TerminalTabs::new();
        let left = TerminalId("left".into());
        let right = TerminalId("right".into());
        tabs.open(left.clone(), "shell 1".into(), 24, 80);
        tabs.open_split(right.clone(), "shell 2".into(), 24, 80);

        assert_eq!(tabs.split_ids(), Some([left.clone(), right.clone()]));
        assert_eq!(tabs.active_id(), Some(right.clone()));

        tabs.focus_id(&left);
        assert_eq!(tabs.active_id(), Some(left.clone()));
        assert_eq!(tabs.split_ids(), Some([left.clone(), right.clone()]));

        tabs.close(&left);
        assert_eq!(tabs.split_ids(), None);
        assert_eq!(tabs.active_id(), Some(right));
    }

    #[test]
    fn selecting_a_hidden_tab_replaces_only_the_focused_split_pane() {
        let mut tabs = TerminalTabs::new();
        let spare = TerminalId("spare".into());
        let left = TerminalId("left".into());
        let right = TerminalId("right".into());
        tabs.open(spare.clone(), "spare".into(), 24, 40);
        tabs.open(left.clone(), "left".into(), 24, 40);
        tabs.open_split(right.clone(), "right".into(), 24, 40);

        tabs.focus(0);
        assert_eq!(tabs.split_ids(), Some([left.clone(), spare.clone()]));
        assert_eq!(tabs.active_id(), Some(spare.clone()));

        tabs.focus_id(&left);
        tabs.focus_id(&right);
        assert_eq!(tabs.split_ids(), Some([right.clone(), spare]));
        assert_eq!(tabs.active_id(), Some(right));
    }

    #[test]
    fn keyboard_tab_navigation_wraps_and_keeps_the_other_split_pane() {
        let mut tabs = TerminalTabs::new();
        let spare = TerminalId("spare".into());
        let left = TerminalId("left".into());
        let right = TerminalId("right".into());
        tabs.open(spare.clone(), "spare".into(), 24, 40);
        tabs.open(left.clone(), "left".into(), 24, 40);
        tabs.open_split(right.clone(), "right".into(), 24, 40);

        assert!(tabs.focus_next());
        assert_eq!(tabs.active_id(), Some(spare.clone()));
        assert_eq!(tabs.split_ids(), Some([left.clone(), spare.clone()]));

        assert!(tabs.focus_previous());
        assert_eq!(tabs.active_id(), Some(right.clone()));
        assert_eq!(tabs.split_ids(), Some([left, right]));

        tabs.close(&spare);
        tabs.close(&TerminalId("left".into()));
        assert!(!tabs.focus_next(), "one tab has nowhere else to move");
    }

    #[test]
    fn the_other_split_pane_can_be_focused_without_changing_the_pair() {
        let mut tabs = TerminalTabs::new();
        let left = TerminalId("left".into());
        let right = TerminalId("right".into());
        tabs.open(left.clone(), "left".into(), 24, 40);
        tabs.open_split(right.clone(), "right".into(), 24, 40);

        assert!(tabs.focus_other_pane());
        assert_eq!(tabs.active_id(), Some(left.clone()));
        assert_eq!(tabs.split_ids(), Some([left.clone(), right.clone()]));
        assert!(tabs.focus_other_pane());
        assert_eq!(tabs.active_id(), Some(right.clone()));
        assert_eq!(tabs.split_ids(), Some([left, right]));

        tabs.unsplit();
        assert!(!tabs.focus_other_pane());
    }

    #[test]
    fn a_split_is_restored_only_while_both_daemon_terminals_still_exist() {
        let mut tabs = TerminalTabs::new();
        let left = TerminalId("left".into());
        let right = TerminalId("right".into());
        tabs.open(left.clone(), "shell 1".into(), 24, 80);
        tabs.open(right.clone(), "shell 2".into(), 24, 80);

        assert!(tabs.restore_split([left.clone(), right.clone()], Some(&left)));
        assert_eq!(tabs.split_ids(), Some([left.clone(), right.clone()]));
        assert_eq!(tabs.active_id(), Some(left));

        tabs.unsplit();
        assert!(!tabs.restore_split([TerminalId("missing".into()), right], None));
        assert_eq!(tabs.split_ids(), None);
    }

    #[test]
    fn closing_a_tab_settles_on_the_one_beside_it() {
        // What every tab strip does: closing the third leaves the second in
        // front, and closing the first leaves whatever is now first.
        let mut tabs = TerminalTabs::new();
        for name in ["one", "two", "three"] {
            tabs.open(TerminalId(name.into()), name.into(), 24, 80);
        }
        tabs.close(&TerminalId("three".into()));
        assert_eq!(tabs.active_id(), Some(TerminalId("two".into())));

        tabs.close(&TerminalId("one".into()));
        assert_eq!(tabs.active_id(), Some(TerminalId("two".into())));

        tabs.close(&TerminalId("two".into()));
        assert!(tabs.is_empty());
        assert_eq!(tabs.active_id(), None, "nothing left to type into");
    }

    #[test]
    fn a_running_terminal_needs_a_second_close_request() {
        let mut tabs = TerminalTabs::new();
        let terminal = TerminalId("one".into());
        tabs.open(terminal.clone(), "shell 1".into(), 24, 80);

        assert_eq!(tabs.request_close(&terminal), CloseRequest::Confirm);
        assert!(tabs.close_confirmation(&terminal));
        assert_eq!(tabs.tabs().len(), 1, "the first press is not destructive");

        assert_eq!(tabs.request_close(&terminal), CloseRequest::Close);
        assert!(tabs.is_empty());
    }

    #[test]
    fn moving_to_another_terminal_cancels_a_pending_close() {
        let mut tabs = TerminalTabs::new();
        let one = TerminalId("one".into());
        tabs.open(one.clone(), "shell 1".into(), 24, 80);
        tabs.open(TerminalId("two".into()), "shell 2".into(), 24, 80);

        assert_eq!(tabs.request_close(&one), CloseRequest::Confirm);
        tabs.focus(1);
        assert!(!tabs.close_confirmation(&one));
        assert_eq!(tabs.request_close(&one), CloseRequest::Confirm);
        assert_eq!(tabs.tabs().len(), 2);
    }

    #[test]
    fn output_lands_on_the_screen_it_belongs_to() {
        let mut tabs = TerminalTabs::new();
        tabs.open(TerminalId("one".into()), "shell 1".into(), 24, 80);
        tabs.open(TerminalId("two".into()), "shell 2".into(), 24, 80);

        assert!(tabs.feed(&TerminalId("one".into()), "behind"));
        tabs.focus(0);
        assert!(
            tabs.active().unwrap().screen.text().contains("behind"),
            "a tab that was not in front still kept what it printed"
        );
        assert!(
            !tabs.feed(&TerminalId("gone".into()), "nowhere"),
            "output for a shell this window is not showing is dropped"
        );
    }

    #[test]
    fn a_reopened_window_adopts_the_shells_that_kept_running() {
        // The point of the daemon owning the pty: the build is still going,
        // and the window that comes back has to find it.
        let mut tabs = TerminalTabs::new();
        let adopted = tabs.adopt(&[info("one", "shell 1"), info("two", "shell 2")], 24, 80);
        assert_eq!(adopted.len(), 2, "both need their history replayed");
        assert_eq!(tabs.active_index(), 0);

        tabs.focus(1);
        tabs.feed(&TerminalId("two".into()), "still building");
        // The daemon says the same shells are running, plus one opened
        // elsewhere; nothing on screen is thrown away for that.
        let adopted = tabs.adopt(
            &[
                info("one", "shell 1"),
                info("two", "shell 2"),
                info("three", "shell 3"),
            ],
            24,
            80,
        );
        assert_eq!(adopted, vec![TerminalId("three".into())]);
        assert_eq!(
            tabs.active_id(),
            Some(TerminalId("two".into())),
            "the tab being read stays in front"
        );
        assert!(
            tabs.active()
                .unwrap()
                .screen
                .text()
                .contains("still building")
        );

        // One of them exited, whoever closed it.
        tabs.adopt(&[info("one", "shell 1")], 24, 80);
        assert_eq!(tabs.tabs().len(), 1);
        assert_eq!(tabs.active_id(), Some(TerminalId("one".into())));
    }
}
