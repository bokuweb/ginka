//! Notebook view rules: tags for the exact filter and notes made from a
//! transcript selection (MonoCode's "selection to note").

use ginka_protocol::model::Note;
use std::collections::BTreeSet;
use std::collections::HashMap;

/// Attachment references named by inline Markdown images, in first-use order.
///
/// References are opaque daemon-owned names. Other image destinations are
/// deliberately excluded so a note cannot make the preview load a remote URL
/// or a path on the client's machine.
pub fn image_references(body: &str) -> Vec<String> {
    let mut references = Vec::new();
    for (_, _, reference) in inline_images(body) {
        if safe_attachment_reference(reference) && !references.iter().any(|item| item == reference)
        {
            references.push(reference.to_string());
        }
    }
    references
}

/// Markdown preview with only verified daemon images restored as image links.
///
/// Every unrecognized `![` and raw HTML tag is escaped, including
/// reference-style Markdown images and HTML `<img>` elements. The caller
/// supplies data URLs only after the daemon has inspected the bytes and
/// identified a supported image signature.
pub fn markdown_with_images(body: &str, loaded: &HashMap<String, String>) -> String {
    let mut rendered = String::with_capacity(body.len());
    let mut consumed = 0;
    for (start, end, reference) in inline_images(body) {
        rendered.push_str(&body[consumed..start].replace("![", "\\!["));
        if safe_attachment_reference(reference) {
            if let Some(url) = loaded.get(reference) {
                let alt = &body[start + 2..end - reference.len() - 3];
                rendered.push_str(&format!("![{alt}]({url})"));
            } else {
                rendered.push_str(&body[start..end].replacen("![", "\\![", 1));
            }
        } else {
            rendered.push_str(&body[start..end].replacen("![", "\\![", 1));
        }
        consumed = end;
    }
    rendered.push_str(&body[consumed..].replace("![", "\\!["));
    // The Markdown renderer also accepts raw HTML `<img src="...">`.
    // Escaping tags keeps that second image path from fetching client URLs.
    rendered.replace('<', "&lt;")
}

fn safe_attachment_reference(reference: &str) -> bool {
    let Some(name) = reference.strip_prefix("ginka-attachment:") else {
        return false;
    };
    !name.is_empty()
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn inline_images(body: &str) -> Vec<(usize, usize, &str)> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = body[cursor..].find("![") {
        let start = cursor + offset;
        let Some(alt_end_offset) = body[start + 2..].find("](") else {
            cursor = start + 2;
            continue;
        };
        let reference_start = start + 2 + alt_end_offset + 2;
        let Some(reference_end_offset) = body[reference_start..].find(')') else {
            cursor = start + 2;
            continue;
        };
        let end = reference_start + reference_end_offset + 1;
        found.push((start, end, &body[reference_start..end - 1]));
        cursor = end;
    }
    found
}

/// Distinct note tags in display order for the notebook's exact-tag filter.
pub fn available_tags(notes: &[Note]) -> Vec<String> {
    notes
        .iter()
        .flat_map(|note| note.tags.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

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
    use std::collections::HashMap;

    #[test]
    fn note_images_only_render_from_loaded_daemon_attachments() {
        let body =
            "See ![plot](ginka-attachment:abc.png) and ![remote](https://example.test/x.png).";
        assert_eq!(image_references(body), vec!["ginka-attachment:abc.png"]);
        let loaded = HashMap::from([(
            "ginka-attachment:abc.png".to_string(),
            "data:image/png;base64,aGVsbG8=".to_string(),
        )]);
        assert_eq!(
            markdown_with_images(body, &loaded),
            "See ![plot](data:image/png;base64,aGVsbG8=) and \\![remote](https://example.test/x.png)."
        );
    }

    #[test]
    fn missing_and_unsafe_image_references_stay_inert() {
        let body = "![missing](ginka-attachment:gone) ![file](file:///tmp/a.png) ![bad](ginka-attachment:../secret) ![shortcut][id] <img src=\"https://example.test/x.png\">";
        assert_eq!(image_references(body), vec!["ginka-attachment:gone"]);
        assert_eq!(
            markdown_with_images(body, &HashMap::new()),
            "\\![missing](ginka-attachment:gone) \\![file](file:///tmp/a.png) \\![bad](ginka-attachment:../secret) \\![shortcut][id] &lt;img src=\"https://example.test/x.png\">"
        );
    }

    #[test]
    fn available_note_tags_are_unique_and_sorted() {
        let note = |id: &str, tags: &[&str]| Note {
            id: id.into(),
            project: None,
            title: String::new(),
            body: String::new(),
            tags: tags.iter().map(|tag| (*tag).into()).collect(),
            created_at: 0,
            updated_at: 0,
        };
        let notes = [
            note("one", &["release", "deploy"]),
            note("two", &["deploy", "ops"]),
        ];
        assert_eq!(available_tags(&notes), vec!["deploy", "ops", "release"]);
        assert!(available_tags(&[]).is_empty());
    }

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
