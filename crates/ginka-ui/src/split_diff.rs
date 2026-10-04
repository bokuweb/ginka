//! The split view of a hunk: the old file on the left, the new on the right,
//! a removed line beside the line that replaced it.
//!
//! git writes a change as a run of removals followed by a run of additions.
//! Read side by side, the n-th removed line of a run faces the n-th added
//! one; what is left over on either side faces nothing. Context lines face
//! themselves. That pairing is the whole of the split view, so it lives here
//! with tests rather than in the view.

use ginka_protocol::model::{DiffLine, Hunk, LineKind};

/// One row of the split view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitRow<'a> {
    /// The line as it was, if this row has one.
    pub left: Option<&'a DiffLine>,
    /// The line as it is now, if this row has one.
    pub right: Option<&'a DiffLine>,
}

/// Lay a hunk out side by side.
pub fn rows(hunk: &Hunk) -> Vec<SplitRow<'_>> {
    let mut rows = Vec::with_capacity(hunk.lines.len());
    let mut removed: Vec<&DiffLine> = Vec::new();
    let mut added: Vec<&DiffLine> = Vec::new();
    for line in &hunk.lines {
        match line.kind {
            LineKind::Removed => removed.push(line),
            LineKind::Added => added.push(line),
            LineKind::Context => {
                flush(&mut rows, &mut removed, &mut added);
                rows.push(SplitRow {
                    left: Some(line),
                    right: Some(line),
                });
            }
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

/// Pair the run held so far, removal against addition, and start a new one.
fn flush<'a>(
    rows: &mut Vec<SplitRow<'a>>,
    removed: &mut Vec<&'a DiffLine>,
    added: &mut Vec<&'a DiffLine>,
) {
    let longest = removed.len().max(added.len());
    let mut left = removed.drain(..);
    let mut right = added.drain(..);
    for _ in 0..longest {
        rows.push(SplitRow {
            left: left.next(),
            right: right.next(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.into(),
            old_line: None,
            new_line: None,
            words: Vec::new(),
            by_agent: None,
        }
    }

    fn hunk(lines: Vec<DiffLine>) -> Hunk {
        Hunk {
            header: "@@ -1 +1 @@".into(),
            lines,
        }
    }

    /// Each row as `left | right`, with `.` for a side that has nothing.
    fn picture(hunk: &Hunk) -> Vec<String> {
        rows(hunk)
            .iter()
            .map(|row| {
                let side = |line: Option<&DiffLine>| {
                    line.map(|line| line.text.clone())
                        .unwrap_or_else(|| ".".into())
                };
                format!("{} | {}", side(row.left), side(row.right))
            })
            .collect()
    }

    use LineKind::*;

    #[test]
    fn context_faces_itself_and_a_replacement_faces_what_it_replaced() {
        let hunk = hunk(vec![
            line(Context, "a"),
            line(Removed, "b"),
            line(Added, "B"),
            line(Context, "c"),
        ]);
        assert_eq!(picture(&hunk), ["a | a", "b | B", "c | c"]);
    }

    #[test]
    fn a_longer_side_faces_nothing_once_the_other_runs_out() {
        let hunk = hunk(vec![
            line(Removed, "one"),
            line(Removed, "two"),
            line(Removed, "three"),
            line(Added, "ONE"),
        ]);
        assert_eq!(picture(&hunk), ["one | ONE", "two | .", "three | ."]);

        let grown = self::hunk(vec![line(Removed, "x"), line(Added, "X"), line(Added, "Y")]);
        assert_eq!(picture(&grown), ["x | X", ". | Y"]);
    }

    #[test]
    fn a_pure_addition_or_removal_has_one_empty_side() {
        let hunk = hunk(vec![
            line(Context, "a"),
            line(Added, "new"),
            line(Context, "b"),
            line(Removed, "gone"),
        ]);
        assert_eq!(picture(&hunk), ["a | a", ". | new", "b | b", "gone | ."]);
    }

    #[test]
    fn runs_separated_by_context_are_paired_separately() {
        let hunk = hunk(vec![
            line(Removed, "r1"),
            line(Context, "c"),
            line(Added, "a1"),
        ]);
        assert_eq!(picture(&hunk), ["r1 | .", "c | c", ". | a1"]);
    }

    #[test]
    fn an_interleaved_run_pairs_in_order_on_each_side() {
        // Not what git writes, but a patch from elsewhere may: removals still
        // face additions in the order each side has them.
        let hunk = hunk(vec![
            line(Removed, "r1"),
            line(Added, "a1"),
            line(Removed, "r2"),
            line(Added, "a2"),
        ]);
        assert_eq!(picture(&hunk), ["r1 | a1", "r2 | a2"]);
    }

    #[test]
    fn nothing_in_is_nothing_out() {
        assert!(rows(&hunk(Vec::new())).is_empty());
    }
}
