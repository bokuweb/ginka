//! Browser address and presentation decisions that do not need a native view.

/// Resolve address-bar input into a URL without treating local development
/// hosts as search terms.
pub fn resolve_address(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(input)
        && matches!(url.scheme(), "http" | "https" | "file" | "about")
    {
        return Some(url.to_string());
    }
    if input.starts_with("localhost")
        || input.starts_with("127.0.0.1")
        || input.starts_with("[::1]")
    {
        return Some(format!("http://{input}"));
    }
    if !input.chars().any(char::is_whitespace) && input.contains('.') {
        return Some(format!("https://{input}"));
    }
    let mut url = url::Url::parse("https://www.google.com/search").expect("static search URL");
    url.query_pairs_mut().append_pair("q", input);
    Some(url.to_string())
}

/// Add an inspected-element bundle without replacing the instruction already
/// being written.
pub fn append_context(draft: &str, context: &str) -> String {
    match (draft.trim(), context.trim()) {
        ("", context) => context.to_string(),
        (draft, "") => draft.to_string(),
        (draft, context) => format!("{draft}\n\n{context}"),
    }
}

/// The script that finds `query` in the page — the next match, or the one
/// before it with `backwards` — wrapping around, ignoring case. The query is
/// passed as a JSON string, so nothing typed can end the call early; `None`
/// for an empty query, which finds nothing worth a round trip.
pub fn find_script(query: &str, backwards: bool) -> Option<String> {
    if query.trim().is_empty() {
        return None;
    }
    let literal = serde_json::Value::String(query.to_string()).to_string();
    Some(format!("window.find({literal}, false, {backwards}, true)"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_search_is_a_find_call_nothing_typed_can_break_out_of() {
        assert_eq!(
            find_script("needle", false).as_deref(),
            Some(r#"window.find("needle", false, false, true)"#)
        );
        assert_eq!(
            find_script("back", true).as_deref(),
            Some(r#"window.find("back", false, true, true)"#)
        );
        let hostile = find_script("\"); alert(1); (\"", false).unwrap();
        assert!(
            hostile.starts_with(r#"window.find("\"); alert(1); (\"""#),
            "{hostile}"
        );
        assert_eq!(hostile.matches("window.find(").count(), 1);
        assert_eq!(find_script("  ", false), None);
    }
    use super::*;

    #[test]
    fn explicit_and_local_urls_navigate_without_becoming_searches() {
        assert_eq!(
            resolve_address("https://example.com/docs"),
            Some("https://example.com/docs".into())
        );
        assert_eq!(
            resolve_address("localhost:5173"),
            Some("http://localhost:5173".into())
        );
        assert_eq!(
            resolve_address("example.com"),
            Some("https://example.com".into())
        );
    }

    #[test]
    fn words_search_and_blank_input_does_nothing() {
        assert!(
            resolve_address("design system spacing")
                .unwrap()
                .contains("q=design+system+spacing")
        );
        assert_eq!(resolve_address("  "), None);
    }

    #[test]
    fn inspected_context_is_appended_without_losing_the_current_instruction() {
        assert_eq!(
            append_context("Increase the spacing", "## Design feedback"),
            "Increase the spacing\n\n## Design feedback"
        );
        assert_eq!(append_context("", "context"), "context");
    }
}
