//! Reading git's unified diff into something a view can draw.
//!
//! waku hands the raw `numstat` and patch text to its client and parses them
//! there. Here it is parsed once, in the crate that can be tested without a
//! window: a diff is domain data, the parsing has real edge cases — renames,
//! binary files, a file with no trailing newline — and every client would
//! otherwise have to get them right again.
//!
//! The parser is deliberately tolerant. `git diff` output varies with the
//! user's own config (`diff.noprefix`, external drivers, `core.autocrlf`), and
//! a file whose header it cannot read is better shown with no hunks than
//! dropped from a review.

use ginka_protocol::model::{ChangeKind, DiffLine, FileChange, Hunk, LineKind, Span};

/// Parse `git diff` output into one entry per file.
pub fn parse(patch: &str) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut current: Option<FileChange> = None;
    let mut hunk: Option<Hunk> = None;
    let mut old_line = 0;
    let mut new_line = 0;

    for line in patch.lines() {
        if let Some(paths) = line.strip_prefix("diff --git ") {
            close(&mut files, &mut current, &mut hunk);
            let (old, new) = split_paths(paths);
            current = Some(FileChange {
                path: new.clone().unwrap_or_default(),
                old_path: old,
                kind: ChangeKind::Modified,
                added: 0,
                removed: 0,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };

        // The header lines between `diff --git` and the first hunk say what
        // kind of change this is, and carry the authoritative paths.
        if let Some(rest) = line.strip_prefix("--- ") {
            file.old_path = strip_prefix_path(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(path) = strip_prefix_path(rest) {
                file.path = path;
            }
            continue;
        }
        if line.starts_with("new file mode") {
            file.kind = ChangeKind::Added;
            continue;
        }
        if line.starts_with("deleted file mode") {
            file.kind = ChangeKind::Deleted;
            continue;
        }
        if line.starts_with("rename from ") || line.starts_with("rename to ") {
            file.kind = ChangeKind::Renamed;
            continue;
        }
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            file.binary = true;
            continue;
        }

        if line.starts_with("@@") {
            if let Some(finished) = hunk.take() {
                file.hunks.push(finished);
            }
            let (old_start, new_start) = hunk_starts(line);
            old_line = old_start;
            new_line = new_start;
            hunk = Some(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
            continue;
        }

        let Some(open) = hunk.as_mut() else {
            continue;
        };
        // Inside a hunk the first character is the marker. A bare empty line is
        // a context line whose trailing space git dropped.
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            // "\ No newline at end of file" is a note about the line above.
            Some('\\') => continue,
            None => (LineKind::Context, ""),
            _ => continue,
        };

        let (old, new) = match kind {
            LineKind::Added => {
                file.added += 1;
                let at = new_line;
                new_line += 1;
                (None, Some(at))
            }
            LineKind::Removed => {
                file.removed += 1;
                let at = old_line;
                old_line += 1;
                (Some(at), None)
            }
            LineKind::Context => {
                let (old, new) = (old_line, new_line);
                old_line += 1;
                new_line += 1;
                (Some(old), Some(new))
            }
        };

        open.lines.push(DiffLine {
            kind,
            text: text.to_string(),
            old_line: old,
            new_line: new,
            words: Vec::new(),
        });
    }

    close(&mut files, &mut current, &mut hunk);
    for file in &mut files {
        for hunk in &mut file.hunks {
            refine(&mut hunk.lines);
        }
    }
    files
}

/// Mark what actually changed inside each replaced line.
///
/// A one-character edit shows as a whole line removed and a whole line added,
/// and finding the character is the reader's job unless someone does it for
/// them. Lines are paired by position within a run, which is what git's own
/// word diff does: the first removed line answers to the first added one.
fn refine(lines: &mut [DiffLine]) {
    let mut index = 0;
    while index < lines.len() {
        let removed_at = index;
        let removed = lines[index..]
            .iter()
            .take_while(|line| line.kind == LineKind::Removed)
            .count();
        let added_at = removed_at + removed;
        let added = lines[added_at..]
            .iter()
            .take_while(|line| line.kind == LineKind::Added)
            .count();
        if removed == 0 || added == 0 {
            index = if removed + added == 0 {
                index + 1
            } else {
                added_at + added
            };
            continue;
        }
        for offset in 0..removed.min(added) {
            let before = lines[removed_at + offset].text.clone();
            let after = lines[added_at + offset].text.clone();
            let (old_words, new_words) = words(&before, &after);
            lines[removed_at + offset].words = old_words;
            lines[added_at + offset].words = new_words;
        }
        index = added_at + added;
    }
}

/// How much of a rewritten line has to survive for the marks to be worth it.
///
/// Below this the two lines have nothing much in common, and marking the
/// difference would be marking the whole line — which says less than leaving
/// it alone.
const KEPT_ENOUGH: f32 = 0.25;

/// The parts of two lines that differ, as byte ranges into each.
fn words(before: &str, after: &str) -> (Vec<Span>, Vec<Span>) {
    let old = tokens(before);
    let new = tokens(after);
    let common = longest_common(&old, &new);
    let kept: usize = common
        .iter()
        .map(|&(index, _)| old[index].1.trim().len())
        .sum();
    let total: usize = old
        .iter()
        .chain(new.iter())
        .map(|token| token.1.trim().len())
        .sum::<usize>()
        .max(1);
    // Both sides are counted, so an unchanged pair scores half; the threshold
    // is against that.
    if (kept * 2) as f32 / total as f32 <= KEPT_ENOUGH {
        return (Vec::new(), Vec::new());
    }

    let old_same: Vec<usize> = common.iter().map(|&(index, _)| index).collect();
    let new_same: Vec<usize> = common.iter().map(|&(_, index)| index).collect();
    (differing(&old, &old_same), differing(&new, &new_same))
}

/// Split a line into words and the runs of anything else between them.
///
/// Punctuation is its own token rather than part of a word, so changing
/// `foo(bar)` to `foo(baz)` marks `bar`, not the whole call.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        let alphanumeric = character.is_alphanumeric() || character == '_';
        let ends = match chars.peek() {
            Some(&(_, next)) => (next.is_alphanumeric() || next == '_') != alphanumeric,
            None => true,
        };
        if ends {
            let end = at + character.len_utf8();
            found.push((start, &line[start..end]));
            start = end;
        }
    }
    found
}

/// The indices of the tokens the two lines have in common, in order.
///
/// A plain LCS. Lines are short — this is one line against one line — so the
/// quadratic table costs nothing and the result is the one a reader expects
/// rather than an approximation of it.
fn longest_common(old: &[(usize, &str)], new: &[(usize, &str)]) -> Vec<(usize, usize)> {
    let mut table = vec![vec![0u16; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i][j] = if old[i].1 == new[j].1 {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut common = Vec::new();
    while i < old.len() && j < new.len() {
        if old[i].1 == new[j].1 {
            common.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    common
}

/// The byte ranges of the tokens that are not in `same`, merged where they
/// touch: three adjacent changed tokens are one mark, not three.
fn differing(all: &[(usize, &str)], same: &[usize]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for (index, (start, text)) in all.iter().enumerate() {
        if same.contains(&index) {
            continue;
        }
        // Whitespace on its own is not worth marking: a mark around a space
        // between two changed words is what makes the run look ragged.
        if text.trim().is_empty() && !spans.is_empty() {
            continue;
        }
        let span = Span {
            start: *start as u32,
            end: (start + text.len()) as u32,
        };
        match spans.last_mut() {
            Some(last) if last.end >= span.start => last.end = span.end,
            _ => spans.push(span),
        }
    }
    spans
}

/// Finish whatever file and hunk are open.
fn close(files: &mut Vec<FileChange>, current: &mut Option<FileChange>, hunk: &mut Option<Hunk>) {
    if let Some(mut file) = current.take() {
        if let Some(open) = hunk.take() {
            file.hunks.push(open);
        }
        files.push(file);
    }
}

/// Split `a/src/main.rs b/src/main.rs` into its two paths.
///
/// Paths with spaces are why this reads from the ends rather than splitting on
/// whitespace: the `a/` and `b/` prefixes are the only reliable markers, and
/// the `---`/`+++` lines correct the result anyway.
fn split_paths(paths: &str) -> (Option<String>, Option<String>) {
    match paths.split_once(" b/") {
        Some((old, new)) => (
            Some(old.trim_start_matches("a/").to_string()),
            Some(new.to_string()),
        ),
        None => (None, None),
    }
}

/// Strip git's `a/` or `b/` prefix, and recognise the absent side.
fn strip_prefix_path(raw: &str) -> Option<String> {
    let path = raw.split('\t').next().unwrap_or(raw).trim();
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_string(),
    )
}

/// The starting line numbers from a `@@ -12,7 +12,9 @@` header.
///
/// A header this cannot read yields 1 on both sides: numbering the lines from
/// somewhere is more useful than refusing to show them.
fn hunk_starts(header: &str) -> (u32, u32) {
    let mut old = 1;
    let mut new = 1;
    for field in header.split_whitespace() {
        if let Some(rest) = field.strip_prefix('-') {
            old = rest
                .split(',')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
        } else if let Some(rest) = field.strip_prefix('+') {
            new = rest
                .split(',')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
        }
    }
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modified_file_carries_its_lines_and_their_numbers() {
        let patch = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,4 +10,5 @@ fn main() {
 let x = 1;
-let y = 2;
+let y = 3;
+let z = 4;
 println!(\"{x}\");
";
        let files = parse(patch);
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.path, "src/main.rs");
        assert_eq!(file.kind, ChangeKind::Modified);
        assert_eq!((file.added, file.removed), (2, 1));

        let lines = &file.hunks[0].lines;
        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!(lines[0].old_line, Some(10));
        assert_eq!(lines[0].new_line, Some(10));
        // A removed line has a number on the old side only, and an added one
        // on the new side only: that is what a comment is anchored to.
        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!((lines[1].old_line, lines[1].new_line), (Some(11), None));
        assert_eq!(lines[2].kind, LineKind::Added);
        assert_eq!((lines[2].old_line, lines[2].new_line), (None, Some(11)));
        assert_eq!(
            lines[4].old_line,
            Some(12),
            "context resumes after the change"
        );
        assert_eq!(lines[4].new_line, Some(13));
    }

    #[test]
    fn a_new_file_is_an_addition_rather_than_a_modification() {
        let patch = "\
diff --git a/notes.md b/notes.md
new file mode 100644
index 000..abc
--- /dev/null
+++ b/notes.md
@@ -0,0 +1,2 @@
+one
+two
";
        let files = parse(patch);
        assert_eq!(files[0].kind, ChangeKind::Added);
        assert_eq!(files[0].path, "notes.md");
        assert_eq!(files[0].old_path, None, "it came from nowhere");
        assert_eq!(files[0].added, 2);
    }

    #[test]
    fn a_deleted_file_says_so_and_keeps_what_was_lost() {
        let patch = "\
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
";
        let files = parse(patch);
        assert_eq!(files[0].kind, ChangeKind::Deleted);
        assert_eq!(files[0].old_path.as_deref(), Some("old.txt"));
        assert_eq!(files[0].removed, 2);
        assert_eq!(files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn a_rename_reads_as_one_move_rather_than_two_files() {
        let patch = "\
diff --git a/src/old.rs b/src/new.rs
similarity index 92%
rename from src/old.rs
rename to src/new.rs
--- a/src/old.rs
+++ b/src/new.rs
@@ -1,2 +1,2 @@
 same
-was
+is
";
        let files = parse(patch);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, ChangeKind::Renamed);
        assert_eq!(files[0].label(), "src/old.rs → src/new.rs");
    }

    #[test]
    fn a_binary_file_is_counted_but_has_nothing_to_read() {
        let patch = "\
diff --git a/logo.png b/logo.png
index abc..def 100644
Binary files a/logo.png and b/logo.png differ
";
        let files = parse(patch);
        assert!(files[0].binary);
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn several_files_are_kept_apart() {
        let patch = "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
-a
+b
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -1 +1 @@
-c
+d
";
        let files = parse(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "one.txt");
        assert_eq!(files[1].path, "two.txt");
        assert_eq!(files[1].hunks[0].lines.len(), 2);
    }

    #[test]
    fn a_note_about_a_missing_newline_is_not_a_line_of_the_file() {
        let patch = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-one
\\ No newline at end of file
+one
";
        let files = parse(patch);
        assert_eq!(files[0].hunks[0].lines.len(), 2);
        assert_eq!((files[0].added, files[0].removed), (1, 1));
    }

    #[test]
    fn a_path_with_a_space_survives_the_split() {
        let patch = "\
diff --git a/my notes.md b/my notes.md
--- a/my notes.md
+++ b/my notes.md
@@ -1 +1 @@
-a
+b
";
        assert_eq!(parse(patch)[0].path, "my notes.md");
    }

    #[test]
    fn output_that_is_not_a_diff_yields_nothing_rather_than_a_wrong_file() {
        assert!(parse("").is_empty());
        assert!(parse("fatal: not a git repository").is_empty());
    }

    #[test]
    fn several_hunks_in_one_file_are_kept_apart() {
        let patch = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
-one
+ONE
@@ -20,2 +20,2 @@
-twenty
+TWENTY
";
        let files = parse(patch);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[1].lines[0].old_line, Some(20));
        assert_eq!((files[0].added, files[0].removed), (2, 2));
    }

    #[test]
    fn a_one_word_edit_is_marked_rather_than_the_whole_line() {
        // The reason word diff exists: a one-character change shows as a whole
        // line removed and a whole line added, and finding it is the reader's
        // job unless someone does it for them.
        let patch = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,1 +1,1 @@
-let total = price * quantity;
+let total = price * amount;
";
        let files = parse(patch);
        let lines = &files[0].hunks[0].lines;
        let removed = &lines[0];
        let added = &lines[1];
        assert_eq!(
            marked(removed),
            vec!["quantity"],
            "only the word that changed"
        );
        assert_eq!(marked(added), vec!["amount"]);
    }

    #[test]
    fn a_line_rewritten_completely_is_left_alone() {
        // Marking all of it says less than marking none of it.
        let patch = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,1 +1,1 @@
-the quick brown fox
+something else entirely different
";
        let files = parse(patch);
        let lines = &files[0].hunks[0].lines;
        assert!(lines[0].words.is_empty(), "{:?}", lines[0].words);
        assert!(lines[1].words.is_empty());
    }

    #[test]
    fn punctuation_is_its_own_word_so_a_call_marks_its_argument() {
        let patch = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,1 @@
-    println!(\"{bar}\");
+    println!(\"{baz}\");
";
        let files = parse(patch);
        let lines = &files[0].hunks[0].lines;
        assert_eq!(marked(&lines[0]), vec!["bar"]);
        assert_eq!(marked(&lines[1]), vec!["baz"]);
    }

    #[test]
    fn a_line_with_no_partner_is_not_marked() {
        // Nothing replaced it, so there is nothing to point at inside it.
        let patch = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,3 @@
 context
+a whole new line
";
        let files = parse(patch);
        let added = &files[0].hunks[0].lines[1];
        assert_eq!(added.kind, LineKind::Added);
        assert!(added.words.is_empty());
    }

    #[test]
    fn several_replaced_lines_are_paired_in_order() {
        let patch = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
-alpha one here
-beta two here
+alpha ONE here
+beta TWO here
";
        let files = parse(patch);
        let lines = &files[0].hunks[0].lines;
        assert_eq!(
            marked(&lines[2]),
            vec!["ONE"],
            "the first added line answers the first removed one"
        );
        assert_eq!(marked(&lines[3]), vec!["TWO"]);
    }

    /// The text under each mark, which is what a reader would see highlighted.
    fn marked(line: &DiffLine) -> Vec<&str> {
        line.words
            .iter()
            .map(|span| &line.text[span.start as usize..span.end as usize])
            .collect()
    }
}
