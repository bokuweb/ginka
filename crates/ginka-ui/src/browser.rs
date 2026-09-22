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

#[cfg(test)]
mod tests {
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
