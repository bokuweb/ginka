//! Notes made from what was read: a transcript selection kept as a note
//! (MonoCode's "selection to note").

/// How long a note's title may be, in characters.
pub const TITLE_LIMIT: usize = 60;

/// A note made from a selection: its title, and its body.
///
/// The title is the selection's first line of words — heading and list
/// marks left off — cut at a word near [`TITLE_LIMIT`]; the body is the
/// selection as a quotation, followed by where it came from when that is
/// known.
pub fn from_selection(text: &str, source: Option<&str>) -> (String, String) {
    let lines: Vec<&str> = text.trim().lines().map(str::trim_end).collect();
    let first = lines
        .iter()
        .map(|line| {
            line.trim_start()
                .trim_start_matches(['#', '-', '*', '>'])
                .trim()
        })
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let title = if first.chars().count() <= TITLE_LIMIT {
        first.to_string()
    } else {
        let cut: String = first.chars().take(TITLE_LIMIT).collect();
        // Back to the last whole word, when there is one to go back to.
        let cut = match cut.rfind(' ') {
            Some(space) if space > TITLE_LIMIT / 2 => &cut[..space],
            _ => cut.as_str(),
        };
        format!("{}…", cut.trim_end())
    };
    let mut body: String = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                ">\n".to_string()
            } else {
                format!("> {line}\n")
            }
        })
        .collect();
    if let Some(source) = source {
        body.push_str(&format!("\n— from {source}\n"));
    }
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_becomes_a_quoted_note_titled_by_its_first_line() {
        let (title, body) = from_selection(
            "\n## Why the cache misses\n- the key includes the time\nso it never hits\n",
            Some("question 0"),
        );
        assert_eq!(title, "Why the cache misses");
        assert_eq!(
            body,
            "> ## Why the cache misses\n> - the key includes the time\n> so it never hits\n\n— from question 0\n"
        );
    }

    #[test]
    fn a_long_first_line_is_cut_at_a_word() {
        let text = "The daemon owns every process, and the window only ever asks it for what it needs to draw";
        let (title, _) = from_selection(text, None);
        assert!(title.chars().count() <= TITLE_LIMIT + 1, "{title}");
        assert!(title.ends_with('…'));
        assert!(text.starts_with(title.trim_end_matches('…').trim_end()));
    }

    #[test]
    fn a_selection_with_no_source_is_just_the_quotation() {
        let (title, body) = from_selection("- one", None);
        assert_eq!(title, "one");
        assert_eq!(body, "> - one\n");
    }
}
