//! Content arriving.
//!
//! A fetch answers and its rows appear, whole, in one frame. That reads as
//! a flicker: nothing moved, so there is nothing to say that anything
//! happened. A short fade says it — the thing was not there and now it is —
//! and costs nothing to read, because it is over before the eye has
//! finished travelling to it.
//!
//! It has to happen once. `with_animation` replays whenever the element's
//! id changes, so the id is what the content *is* — the item, the file, the
//! job — and not anything that changes while it is on screen.

use e1_ui::Tokens;
use gpui::{
    AnimationExt as _, AnyElement, App, ElementId, IntoElement, ParentElement as _, Styled as _,
    div,
};

/// Fade something in over the theme's fade duration.
pub fn fade_in(id: impl Into<ElementId>, element: AnyElement, cx: &App) -> AnyElement {
    let duration = Tokens::global(cx).duration_ms.fade();
    // Wrapped, because opacity is a style and an `AnyElement` has none.
    // The wrapper fills its slot, which is what every caller's body does.
    div()
        .size_full()
        .child(element)
        .with_animation(
            id,
            gpui::Animation::new(duration).with_easing(gpui::ease_out_quint()),
            |element, delta| element.opacity(delta),
        )
        .into_any_element()
}

/// The same, when there is something to fade.
///
/// A body that is still loading, or that failed, is left alone: a skeleton
/// fading in and then out again is two flickers where there were none.
pub fn fade_when(
    ready: bool,
    id: impl Into<ElementId>,
    element: AnyElement,
    cx: &App,
) -> AnyElement {
    match ready {
        true => fade_in(id, element, cx),
        false => element,
    }
}
