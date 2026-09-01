//! The right panel: `docs/ui.md` §3.4.
//!
//! A surface is whatever the user wants beside the transcript — a terminal, git,
//! files, an editor, later a browser. M0 renders the empty state and the
//! chooser; M4 turns this into a `DockArea` so surfaces can be dragged, split
//! and persisted per workspace.

use ginka_ui::Tokens;
use ginka_ui::surface::Surface;
use gpui::*;
use gpui_component::{Icon, IconName, h_flex, v_flex};

pub struct SurfacePanel {
    open: Option<Surface>,
}

impl SurfacePanel {
    pub fn new() -> Self {
        Self { open: None }
    }

    fn toolbar(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .justify_between()
            .items_center()
            .child(
                Icon::new(IconName::Plus)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Maximize)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    )
                    .child(
                        Icon::new(IconName::PanelRight)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    ),
            )
    }

    fn chooser_button(&self, surface: Surface, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .id(surface.label())
            .w_full()
            .px_3p5()
            .py_3()
            .gap_2p5()
            .items_center()
            .rounded(px(tokens.radius.panel))
            .bg(tokens.colors().bg_surface)
            .border_1()
            .border_color(tokens.colors().border_subtle)
            .hover(|this| this.bg(tokens.colors().bg_raised))
            .child(
                surface
                    .icon()
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.colors().text_primary)
                    .child(surface.label()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open = Some(surface);
                cx.notify();
            }))
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_5()
            .px_8()
            .child(
                v_flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .text_lg()
                            .text_color(tokens.colors().text_primary)
                            .child(rust_i18n::t!("surface.empty.title").to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("surface.empty.hint").to_string()),
                    ),
            )
            .child(
                v_flex().w_full().max_w(px(360.)).gap_2().children(
                    Surface::ALL
                        .iter()
                        .map(|surface| self.chooser_button(*surface, cx).into_any_element()),
                ),
            )
    }

    fn placeholder(&self, surface: Surface, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                surface
                    .icon()
                    .size_6()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.colors().text_secondary)
                    .child(surface.label()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(surface.availability()),
            )
    }
}

impl Render for SurfacePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let border = tokens.colors().border_subtle;
        let open = self.open;

        v_flex()
            .size_full()
            .border_l_1()
            .border_color(border)
            .child(self.toolbar(cx))
            .child(match open {
                None => self.empty_state(cx).into_any_element(),
                Some(surface) => self.placeholder(surface, cx).into_any_element(),
            })
    }
}
