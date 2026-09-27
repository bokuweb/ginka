//! A person's picture, or their initial while it is on its way.
//!
//! Avatars are fetched by the store and kept as files, because GPUI draws
//! an image from a path and this app has no HTTP client the window could
//! hand it. Until the file is there — or when the source cannot fetch one,
//! as the scripted one cannot — the initial in a tinted circle stands in,
//! so a row never has a hole in it.

use e1_ui::Tokens;
use gpui::*;
use std::path::PathBuf;

/// A round picture `size` across, or the initial of `login`.
pub fn avatar(path: Option<PathBuf>, login: &str, size: Pixels, cx: &App) -> AnyElement {
    let tokens = Tokens::global(cx);
    match path {
        Some(path) => img(path)
            .size(size)
            .flex_shrink_0()
            .rounded_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => {
            let initial: SharedString = login
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase().to_string())
                .unwrap_or_default()
                .into();
            div()
                .size(size)
                .flex_shrink_0()
                .rounded_full()
                .bg(tokens.colors().row_active())
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.5))
                .text_color(tokens.colors().text_primary)
                .child(initial)
                .into_any_element()
        }
    }
}
