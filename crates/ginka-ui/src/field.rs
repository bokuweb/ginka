//! Text fields whose focus is a hairline rather than a ring.
//!
//! The toolkit marks a focused field with a 3px ring painted outside its 1px
//! border, which at this UI's density reads as a heavy outline. Buttons keep
//! that ring — a ghost button has no border to tint — so the theme leaves it
//! on and fields opt out here instead.

use gpui::{
    App, Entity, Focusable as _, IntoElement, RenderOnce, Styled as _, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _,
    input::{Input, InputState},
};

/// A single-line field that shows focus by tinting its own border.
#[derive(IntoElement)]
pub struct Field {
    state: Entity<InputState>,
}

/// A [`Field`] over `state`.
pub fn input(state: &Entity<InputState>) -> Field {
    Field {
        state: state.clone(),
    }
}

impl RenderOnce for Field {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focused = self
            .state
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        Input::new(&self.state)
            .focus_bordered(false)
            .when(focused, |input| input.border_color(cx.theme().ring))
    }
}
