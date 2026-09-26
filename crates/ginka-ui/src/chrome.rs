//! Pieces of window chrome that appear in more than one place, so the same
//! thing looks the same wherever it is: a tab in the conversation strip, the
//! right panel, the terminal and the editor is one shape (`docs/ui.md` §2).

use crate::Tokens;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Div, ElementId, InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    Styled as _, div, px,
};
use gpui_component::{Icon, IconName, Sizable as _, h_flex};

/// How tall a tab is.
pub const TAB_HEIGHT: f32 = 26.;

/// A tab: a rounded row; the one in front is a card, filled and outlined.
///
/// The caller adds what the tab holds — usually [`tab_icon`], [`tab_label`]
/// and [`tab_close`] — and what a press does.
pub fn tab(id: impl Into<ElementId>, active: bool, cx: &App) -> Stateful<Div> {
    let tokens = Tokens::global(cx);
    let colors = tokens.colors();
    let (hover, active_bg, active_border) =
        (colors.row_hover(), colors.bg_surface, colors.border_subtle);
    h_flex()
        .id(id)
        .h(px(TAB_HEIGHT))
        .flex_shrink_0()
        .pl(px(8.))
        .pr(px(4.))
        .gap(px(6.))
        .items_center()
        .rounded(px(tokens.radius.row))
        .text_size(px(12.5))
        .text_color(if active {
            colors.text_primary
        } else {
            colors.text_secondary
        })
        .cursor_pointer()
        // Every tab carries a hairline, clear unless it is in front, so the
        // one in front is no bigger than the rest.
        .border_1()
        .border_color(if active {
            active_border
        } else {
            gpui::transparent_black()
        })
        .when(active, |this| this.bg(active_bg))
        .when(!active, |this| this.hover(move |this| this.bg(hover)))
}

/// A tab's mark, quieter than its label.
pub fn tab_icon(icon: Icon, active: bool, cx: &App) -> Icon {
    let colors = Tokens::global(cx).colors();
    icon.with_size(px(13.)).text_color(if active {
        colors.text_secondary
    } else {
        colors.text_muted
    })
}

/// A tab's name, cut short at the end rather than widening the strip.
pub fn tab_label(text: impl Into<SharedString>) -> Div {
    div().min_w_0().truncate().child(text.into())
}

/// A tab's close control; the caller says what closing does.
pub fn tab_close(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let tokens = Tokens::global(cx);
    let colors = tokens.colors();
    let hover = colors.row_active();
    div()
        .id(id)
        .flex_shrink_0()
        .p(px(2.))
        .rounded(px(tokens.radius.control()))
        .cursor_pointer()
        .hover(move |this| this.bg(hover))
        .child(
            Icon::new(IconName::Close)
                .with_size(px(11.))
                .text_color(colors.text_muted),
        )
}
