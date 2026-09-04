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
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

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
        let cursor = self.term.grid().cursor.point;
        let display_offset = self.term.grid().display_offset();
        let mut screen = Vec::with_capacity(self.rows as usize);

        for line in 0..self.rows as usize {
            let mut row = Vec::with_capacity(self.cols as usize);
            for column in 0..self.cols as usize {
                let point = Point::new(Line(line as i32), Column(column));
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_screen_keeps_its_trailing_blanks_for_the_view_to_paint() {
        let mut screen = TerminalScreen::new(2, 8);
        screen.feed("ab");
        assert_eq!(
            screen.rows_of_cells()[0].len(),
            8,
            "a shell that painted a bar across the width would lose its end"
        );
    }
}
