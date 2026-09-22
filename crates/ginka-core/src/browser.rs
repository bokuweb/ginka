//! Safe, bounded context captured from an untrusted browser page.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maximum retained element text, feedback, and one nearby text item.
pub const TEXT_LIMIT: usize = 2_048;
/// Maximum retained HTML around the selected element.
pub const HTML_LIMIT: usize = 8_192;
/// Maximum nearby text rows accepted from the page.
pub const NEARBY_TEXT_LIMIT: usize = 10;
/// Maximum selector or source-location length.
pub const LOCATOR_LIMIT: usize = 1_024;

const SAFE_ATTRIBUTES: &[&str] = &[
    "id",
    "class",
    "name",
    "type",
    "role",
    "href",
    "src",
    "alt",
    "title",
    "placeholder",
    "for",
    "action",
    "method",
];

const SECRET_MARKERS: &[&str] = &[
    "access_token",
    "auth_token",
    "api_key",
    "apikey",
    "client_secret",
    "oauth_state",
    "x-amz-",
    "session_id",
    "sessionid",
    "csrf",
    "secret",
    "password",
    "passwd",
];

/// Context captured at the instant an element is selected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserCapture {
    /// Page-level context with credentials removed from its URL.
    pub page: BrowserPage,
    /// The selected DOM element.
    pub element: BrowserElement,
    /// Short text snippets around the selection.
    pub nearby_text: Vec<String>,
    /// Optional daemon-owned image attachment containing the visual context.
    pub screenshot_reference: Option<String>,
}

/// Page metadata retained with a browser selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserPage {
    /// URL without a query string or fragment.
    pub url: String,
    /// Visible document title.
    pub title: String,
    /// Viewport width in CSS pixels.
    pub viewport_width: f64,
    /// Viewport height in CSS pixels.
    pub viewport_height: f64,
}

/// One inspected DOM element and the small context an agent needs to find it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserElement {
    /// Lowercase HTML tag name.
    pub tag_name: String,
    /// Best-effort unique CSS selector.
    pub selector: String,
    /// Visible text from the element.
    pub text: String,
    /// Bounded outer HTML plus its immediate neighborhood.
    pub html: String,
    /// Development source location when the page exposes one.
    pub source: Option<String>,
    /// Allowlisted DOM attributes.
    pub attributes: BTreeMap<String, String>,
    /// Accessibility name exposed by the page.
    pub accessibility_name: Option<String>,
    /// Viewport-relative element rectangle in CSS pixels.
    pub bounds: BrowserRect,
    /// Curated computed style values useful for visual changes.
    pub styles: BrowserStyles,
}

/// A rectangle in browser CSS pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserRect {
    /// Left edge relative to the viewport.
    pub x: f64,
    /// Top edge relative to the viewport.
    pub y: f64,
    /// Width in CSS pixels.
    pub width: f64,
    /// Height in CSS pixels.
    pub height: f64,
}

/// Curated computed CSS rather than a page-controlled unbounded property map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserStyles {
    /// Computed display mode.
    pub display: String,
    /// Computed position mode.
    pub position: String,
    /// Computed margin shorthand.
    pub margin: String,
    /// Computed padding shorthand.
    pub padding: String,
    /// Computed foreground colour.
    pub color: String,
    /// Computed background colour.
    pub background: String,
    /// Computed border shorthand.
    pub border: String,
    /// Computed border radius.
    pub border_radius: String,
    /// Computed font family.
    pub font_family: String,
    /// Computed font size.
    pub font_size: String,
    /// Computed font weight.
    pub font_weight: String,
    /// Computed line height.
    pub line_height: String,
    /// Computed text alignment.
    pub text_align: String,
}

/// Re-validate page-controlled context before it reaches application chrome or
/// an agent prompt.
pub fn sanitize_capture(mut capture: BrowserCapture) -> BrowserCapture {
    capture.page.url = sanitize_url(&capture.page.url);
    capture.page.title = bounded(&capture.page.title, TEXT_LIMIT);
    capture.page.viewport_width = finite_non_negative(capture.page.viewport_width);
    capture.page.viewport_height = finite_non_negative(capture.page.viewport_height);

    let element = &mut capture.element;
    element.tag_name = safe_metadata(&element.tag_name, 64);
    element.selector = safe_metadata(&element.selector, LOCATOR_LIMIT);
    element.text = safe_metadata(&element.text, TEXT_LIMIT);
    element.html = safe_metadata(&element.html, HTML_LIMIT);
    element.source = element
        .source
        .take()
        .map(|source| safe_metadata(&source, LOCATOR_LIMIT))
        .filter(|source| !source.is_empty());
    element.accessibility_name = element
        .accessibility_name
        .take()
        .map(|name| safe_metadata(&name, TEXT_LIMIT))
        .filter(|name| !name.is_empty());
    element.attributes = element
        .attributes
        .iter()
        .filter_map(|(name, value)| {
            let name = name.to_ascii_lowercase();
            if !SAFE_ATTRIBUTES.contains(&name.as_str()) && !name.starts_with("aria-") {
                return None;
            }
            let mut value = safe_metadata(value, TEXT_LIMIT);
            if matches!(name.as_str(), "href" | "src" | "action") && value != "[redacted]" {
                value = sanitize_url(&value);
            }
            Some((name, value))
        })
        .collect();
    element.bounds = BrowserRect {
        x: finite(element.bounds.x),
        y: finite(element.bounds.y),
        width: finite_non_negative(element.bounds.width),
        height: finite_non_negative(element.bounds.height),
    };
    sanitize_styles(&mut element.styles);
    capture.nearby_text = capture
        .nearby_text
        .into_iter()
        .take(NEARBY_TEXT_LIMIT)
        .map(|text| safe_metadata(&text, TEXT_LIMIT))
        .filter(|text| !text.is_empty())
        .collect();
    capture.screenshot_reference = capture
        .screenshot_reference
        .filter(|reference| reference.starts_with("ginka-attachment:"))
        .map(|reference| bounded(&reference, LOCATOR_LIMIT));
    capture
}

/// Format one browser selection and its requested change as a single prompt.
pub fn format_capture(capture: &BrowserCapture, feedback: &str) -> String {
    let capture = sanitize_capture(capture.clone());
    let path = url::Url::parse(&capture.page.url)
        .ok()
        .map(|url| url.path().to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| capture.page.url.clone());
    let mut lines = vec![
        format!("## Design feedback: {path}"),
        String::new(),
        format!("URL: {}", capture.page.url),
        format!(
            "Viewport: {}x{}",
            capture.page.viewport_width.round(),
            capture.page.viewport_height.round()
        ),
        format!("Element: <{}>", capture.element.tag_name),
        format!("Selector: `{}`", inline_code(&capture.element.selector)),
        format!(
            "Bounds: x={}, y={}, {}x{}",
            capture.element.bounds.x.round(),
            capture.element.bounds.y.round(),
            capture.element.bounds.width.round(),
            capture.element.bounds.height.round()
        ),
    ];
    if let Some(source) = &capture.element.source {
        lines.push(format!("Source: {source}"));
    }
    if let Some(name) = &capture.element.accessibility_name {
        lines.push(format!("Accessible name: {name}"));
    }
    if !capture.element.text.is_empty() {
        lines.push(format!("Text: {}", capture.element.text));
    }
    let styles = style_lines(&capture.element.styles);
    if !styles.is_empty() {
        lines.push(String::new());
        lines.push("Computed styles:".into());
        lines.extend(styles);
    }
    if !capture.element.html.is_empty() {
        let marker = fence_marker(&capture.element.html);
        lines.extend([
            String::new(),
            format!("{marker}html"),
            capture.element.html.clone(),
            marker,
        ]);
    }
    lines.extend([
        String::new(),
        format!("Requested change: {}", safe_metadata(feedback, TEXT_LIMIT)),
    ]);
    if let Some(reference) = &capture.screenshot_reference {
        lines.extend([String::new(), reference.clone()]);
    }
    lines.join("\n")
}

fn sanitize_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return String::new();
    };
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return String::new();
    }
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

fn safe_metadata(value: &str, limit: usize) -> String {
    let value = bounded(value, limit);
    let lower = value.to_ascii_lowercase();
    if SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) {
        "[redacted]".into()
    } else {
        value
    }
}

fn bounded(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= limit)
        .last()
        .unwrap_or(0);
    value[..end].to_string()
}

fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn finite_non_negative(value: f64) -> f64 {
    finite(value).max(0.0)
}

fn sanitize_styles(styles: &mut BrowserStyles) {
    for value in [
        &mut styles.display,
        &mut styles.position,
        &mut styles.margin,
        &mut styles.padding,
        &mut styles.color,
        &mut styles.background,
        &mut styles.border,
        &mut styles.border_radius,
        &mut styles.font_family,
        &mut styles.font_size,
        &mut styles.font_weight,
        &mut styles.line_height,
        &mut styles.text_align,
    ] {
        *value = safe_metadata(value, 512);
    }
}

fn style_lines(styles: &BrowserStyles) -> Vec<String> {
    [
        ("display", &styles.display),
        ("position", &styles.position),
        ("margin", &styles.margin),
        ("padding", &styles.padding),
        ("color", &styles.color),
        ("background", &styles.background),
        ("border", &styles.border),
        ("border-radius", &styles.border_radius),
        ("font-family", &styles.font_family),
        ("font-size", &styles.font_size),
        ("font-weight", &styles.font_weight),
        ("line-height", &styles.line_height),
        ("text-align", &styles.text_align),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty() && value.as_str() != "normal")
    .map(|(name, value)| format!("- {name}: {value}"))
    .collect()
}

fn inline_code(content: &str) -> String {
    content.replace('`', "ˋ")
}

fn fence_marker(content: &str) -> String {
    let mut longest = 2;
    let mut run = 0;
    for byte in content.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> BrowserCapture {
        BrowserCapture {
            page: BrowserPage {
                url: "https://example.com/pricing?access_token=secret#plans".into(),
                title: "Pricing".into(),
                viewport_width: 1280.0,
                viewport_height: 720.0,
            },
            element: BrowserElement {
                tag_name: "button".into(),
                selector: "main > button.primary".into(),
                text: "Start free trial".into(),
                html: "<button class=\"primary\">Start free trial</button>".into(),
                source: Some("src/PricingButton.tsx:42:8".into()),
                attributes: [
                    ("class".into(), "primary".into()),
                    ("onclick".into(), "steal()".into()),
                    ("data-token".into(), "secret".into()),
                ]
                .into(),
                accessibility_name: Some("Start free trial".into()),
                bounds: BrowserRect {
                    x: 400.0,
                    y: 300.0,
                    width: 148.0,
                    height: 44.0,
                },
                styles: BrowserStyles {
                    display: "inline-flex".into(),
                    padding: "12px 24px".into(),
                    color: "rgb(255, 255, 255)".into(),
                    background: "rgb(99, 102, 241)".into(),
                    font_size: "16px".into(),
                    ..BrowserStyles::default()
                },
            },
            nearby_text: vec!["Pro".into(), "$29/month".into()],
            screenshot_reference: Some("ginka-attachment:pricing.png".into()),
        }
    }

    #[test]
    fn untrusted_browser_context_is_bounded_redacted_and_url_sanitized() {
        let mut raw = capture();
        raw.element.text = "x".repeat(10_000);
        raw.element.selector = "button[aria-label='client_secret=bad']".into();
        raw.nearby_text = (0..30).map(|index| format!("nearby {index}")).collect();

        let safe = sanitize_capture(raw);

        assert_eq!(safe.page.url, "https://example.com/pricing");
        assert_eq!(safe.element.selector, "[redacted]");
        assert!(safe.element.text.len() <= TEXT_LIMIT);
        assert_eq!(safe.element.attributes.len(), 1);
        assert_eq!(safe.element.attributes["class"], "primary");
        assert_eq!(safe.nearby_text.len(), NEARBY_TEXT_LIMIT);
    }

    #[test]
    fn browser_context_becomes_one_agent_prompt_with_the_image_reference() {
        let prompt = format_capture(&capture(), "Increase this padding to match the cards.");

        assert!(prompt.contains("Design feedback: /pricing"));
        assert!(prompt.contains("`main > button.primary`"));
        assert!(prompt.contains("src/PricingButton.tsx:42:8"));
        assert!(prompt.contains("padding: 12px 24px"));
        assert!(prompt.contains("Increase this padding"));
        assert!(prompt.contains("ginka-attachment:pricing.png"));
    }
}
