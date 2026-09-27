//! A unified diff, line by line, ready to draw.
//!
//! GitHub sends a file's `patch` as hunks with no `---`/`+++` header. A view
//! that walked the text on every frame would be re-parsing a diff to draw
//! it, so it is walked once here and the line numbers are computed as it
//! goes, which is the one piece of state a diff view needs and the text
//! does not carry.

/// What a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A `@@ -a,b +c,d @@` header.
    Hunk,
    /// Unchanged, shown for context.
    Context,
    /// Added by the change.
    Added,
    /// Removed by the change.
    Removed,
}

/// One line of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// What it is.
    pub kind: Kind,
    /// Its number in the old file, when it was there.
    pub old: Option<u32>,
    /// Its number in the new file, when it is there.
    pub new: Option<u32>,
    /// The text, without the leading marker.
    pub text: String,
}

/// Walk a patch into lines.
///
/// A line that is none of the four kinds (a `\ No newline at end of file`
/// note) is kept as context with no numbers, so nothing GitHub sent is
/// dropped on the floor.
pub fn parse(patch: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("@@") {
            let (start_old, start_new) = hunk_starts(rest);
            old = start_old;
            new = start_new;
            lines.push(Line {
                kind: Kind::Hunk,
                old: None,
                new: None,
                text: raw.to_string(),
            });
            continue;
        }
        let (kind, text) = match raw.chars().next() {
            Some('+') => (Kind::Added, &raw[1..]),
            Some('-') => (Kind::Removed, &raw[1..]),
            Some(' ') => (Kind::Context, &raw[1..]),
            Some('\\') => {
                lines.push(Line {
                    kind: Kind::Context,
                    old: None,
                    new: None,
                    text: raw.to_string(),
                });
                continue;
            }
            _ => (Kind::Context, raw),
        };
        let (old_number, new_number) = match kind {
            Kind::Added => {
                new += 1;
                (None, Some(new))
            }
            Kind::Removed => {
                old += 1;
                (Some(old), None)
            }
            _ => {
                old += 1;
                new += 1;
                (Some(old), Some(new))
            }
        };
        lines.push(Line {
            kind,
            old: old_number,
            new: new_number,
            text: text.to_string(),
        });
    }
    lines
}

/// The starting line numbers out of ` -a,b +c,d @@ …`. A missing or
/// malformed header counts from one rather than failing: the text is still
/// worth drawing, only the numbers would be off.
fn hunk_starts(header: &str) -> (u32, u32) {
    let mut parts = header.split_whitespace();
    let old = parts
        .next()
        .and_then(|part| part.strip_prefix('-'))
        .and_then(|range| range.split(',').next())
        .and_then(|start| start.parse().ok())
        .unwrap_or(1u32);
    let new = parts
        .next()
        .and_then(|part| part.strip_prefix('+'))
        .and_then(|range| range.split(',').next())
        .and_then(|start| start.parse().ok())
        .unwrap_or(1u32);
    // The counters are incremented before use, so they start one below.
    (old.saturating_sub(1), new.saturating_sub(1))
}

/// One row of a side-by-side view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitRow {
    /// A hunk header, across both sides.
    Hunk(String),
    /// The old file's line on the left, the new file's on the right. A
    /// context line is on both; an addition has no left; a removal no
    /// right; a removal paired with the addition that replaced it has both.
    Pair {
        /// The old file's line.
        left: Option<Line>,
        /// The new file's line.
        right: Option<Line>,
    },
}

/// Lay the lines out side by side.
///
/// Removals and the additions that follow them in the same run are
/// paired off in order, which is what makes a changed line read as one
/// row with its before on the left and its after on the right; the
/// leftovers of the longer run stand alone. Context lines sit on both
/// sides.
pub fn split(lines: &[Line]) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let mut removed: Vec<Line> = Vec::new();
    let mut added: Vec<Line> = Vec::new();
    let flush = |rows: &mut Vec<SplitRow>, removed: &mut Vec<Line>, added: &mut Vec<Line>| {
        let count = removed.len().max(added.len());
        let mut removed = removed.drain(..);
        let mut added = added.drain(..);
        for _ in 0..count {
            rows.push(SplitRow::Pair {
                left: removed.next(),
                right: added.next(),
            });
        }
    };
    for line in lines {
        match line.kind {
            Kind::Removed => removed.push(line.clone()),
            Kind::Added => added.push(line.clone()),
            Kind::Hunk => {
                flush(&mut rows, &mut removed, &mut added);
                rows.push(SplitRow::Hunk(line.text.clone()));
            }
            Kind::Context => {
                flush(&mut rows, &mut removed, &mut added);
                rows.push(SplitRow::Pair {
                    left: Some(line.clone()),
                    right: Some(line.clone()),
                });
            }
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_line_reads_as_one_row_and_the_leftovers_stand_alone() {
        let rows = split(&parse(PATCH));
        assert_eq!(
            rows[0],
            SplitRow::Hunk("@@ -10,3 +10,4 @@ fn main() {".into())
        );
        // context: both sides
        assert!(
            matches!(&rows[1], SplitRow::Pair { left: Some(l), right: Some(r) } if l.text == "context" && r.text == "context")
        );
        // "gone" replaced by "here": one row
        assert!(
            matches!(&rows[2], SplitRow::Pair { left: Some(l), right: Some(r) } if l.text == "gone" && r.text == "here")
        );
        // the second addition has no partner
        assert!(
            matches!(&rows[3], SplitRow::Pair { left: None, right: Some(r) } if r.text == "and here")
        );
        assert!(matches!(
            &rows[4],
            SplitRow::Pair {
                left: Some(_),
                right: Some(_)
            }
        ));
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn a_removal_with_nothing_after_it_keeps_its_side() {
        let rows = split(&parse("@@ -1,2 +1 @@\n a\n-b"));
        assert!(matches!(&rows[2], SplitRow::Pair { left: Some(l), right: None } if l.text == "b"));
    }

    const PATCH: &str = "@@ -10,3 +10,4 @@ fn main() {\n context\n-gone\n+here\n+and here\n more";

    #[test]
    fn lines_are_numbered_from_the_hunk_header() {
        let lines = parse(PATCH);
        assert_eq!(lines[0].kind, Kind::Hunk);
        assert_eq!(
            lines[1],
            Line {
                kind: Kind::Context,
                old: Some(10),
                new: Some(10),
                text: "context".into()
            }
        );
        assert_eq!(
            lines[2],
            Line {
                kind: Kind::Removed,
                old: Some(11),
                new: None,
                text: "gone".into()
            }
        );
        assert_eq!(
            lines[3],
            Line {
                kind: Kind::Added,
                old: None,
                new: Some(11),
                text: "here".into()
            }
        );
        assert_eq!(
            lines[4],
            Line {
                kind: Kind::Added,
                old: None,
                new: Some(12),
                text: "and here".into()
            }
        );
        assert_eq!(
            lines[5],
            Line {
                kind: Kind::Context,
                old: Some(12),
                new: Some(13),
                text: "more".into()
            }
        );
    }

    #[test]
    fn a_second_hunk_restarts_the_numbers() {
        let lines = parse("@@ -1 +1 @@\n-a\n+b\n@@ -40,2 +40,3 @@\n keep\n+new");
        assert_eq!(lines[4].old, Some(40));
        assert_eq!(lines[5].new, Some(41));
    }

    #[test]
    fn the_no_newline_note_is_kept_without_a_number() {
        let lines = parse("@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file");
        let note = lines.last().unwrap();
        assert_eq!(note.kind, Kind::Context);
        assert_eq!((note.old, note.new), (None, None));
        assert!(note.text.starts_with('\\'));
    }

    #[test]
    fn a_broken_header_counts_from_one_rather_than_failing() {
        let lines = parse("@@ nonsense @@\n+x");
        assert_eq!(lines[1].new, Some(1));
    }
}
