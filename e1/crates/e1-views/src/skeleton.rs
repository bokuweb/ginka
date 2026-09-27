//! What a column shows while its first answer is on the way.
//!
//! Bars in the shape of what is coming, pulsing, rather than a word: a
//! reader who sees the shape of a list knows a list is coming and where
//! to look for it, and *Loading…* tells them only that they are waiting.
//! These stand in only for a first load — a refresh keeps the old value on
//! screen (`e1_ui::Fetch`) and needs nothing here.

use e1_ui::Tokens;
use gpui::StatefulInteractiveElement as _;
use gpui::*;
use gpui_component::skeleton::Skeleton;
use gpui_component::{h_flex, v_flex};

/// A bar `width` wide and `height` tall, rounded like a row.
fn bar(width: Pixels, height: Pixels, cx: &App) -> Skeleton {
    let tokens = Tokens::global(cx);
    Skeleton::new()
        .w(width)
        .h(height)
        .rounded(px(tokens.radius.control()))
}

/// The shape of `count` 56 px item rows, including their right-aligned number.
pub fn list_rows(count: usize, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .overflow_hidden()
        .py_1()
        .children((0..count).map(|index| {
            // Use the row's available width: the embedded Inbox column is
            // much narrower than e1's standalone centre pane.
            let title = [0.85, 0.62, 0.76, 0.55, 0.9, 0.68][index % 6];
            h_flex()
                .w_full()
                .h(px(56.))
                .px_4()
                .gap_2p5()
                .items_center()
                .overflow_hidden()
                .child(Skeleton::new().size_4().rounded_full().flex_shrink_0())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .items_center()
                                .overflow_hidden()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Skeleton::new()
                                            .w(relative(title))
                                            .h(px(12.))
                                            .rounded(px(3.)),
                                    ),
                                )
                                .child(bar(px(10.), px(10.), cx).secondary())
                                .child(bar(px(28.), px(9.), cx).secondary()),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_1p5()
                                .items_center()
                                .overflow_hidden()
                                .child(bar(px(58.), px(9.), cx).secondary())
                                .child(bar(px(30.), px(9.), cx).secondary())
                                .child(bar(px(18.), px(9.), cx).secondary())
                                .child(bar(px(36.), px(13.), cx).secondary()),
                        ),
                )
        }))
        .into_any_element()
}

/// The shape of `count` 60 px Project table rows with a status at the right.
pub fn project_rows(count: usize, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .overflow_hidden()
        .children((0..count).map(|index| {
            let title = [0.76, 0.58, 0.88, 0.66, 0.72][index % 5];
            h_flex()
                .w_full()
                .h(px(60.))
                .px_4()
                .gap_2p5()
                .items_center()
                .overflow_hidden()
                .child(Skeleton::new().size_4().rounded_full().flex_shrink_0())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(Skeleton::new().w(relative(title)).h(px(12.)))
                        .child(Skeleton::new().w(px(86.)).h(px(10.)).secondary()),
                )
                .child(bar(px(72.), px(19.), cx).secondary())
        }))
        .into_any_element()
}

/// The shape of `count` 52 px commit rows with a graph rail and metadata.
pub fn history_rows(count: usize, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .overflow_hidden()
        .children((0..count).map(|index| {
            let title = [0.8, 0.56, 0.72, 0.91, 0.64][index % 5];
            h_flex()
                .w_full()
                .h(px(52.))
                .px_2()
                .items_center()
                .overflow_hidden()
                .child(
                    h_flex()
                        .w(px(26.))
                        .h_full()
                        .pl(px(4.))
                        .items_center()
                        .flex_shrink_0()
                        .child(Skeleton::new().size(px(8.)).rounded_full()),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(Skeleton::new().w(relative(title)).h(px(12.)))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(bar(px(72.), px(9.), cx).secondary())
                                .child(bar(px(32.), px(9.), cx).secondary())
                                .child(bar(px(48.), px(9.), cx).secondary()),
                        ),
                )
        }))
        .into_any_element()
}

/// The shape of `count` file paths.
pub fn path_rows(count: usize, _cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .overflow_hidden()
        .py_1()
        .children((0..count).map(|index| {
            let width = [0.72, 0.48, 0.83, 0.58, 0.76][index % 5];
            h_flex()
                .w_full()
                .h(px(28.))
                .px_4()
                .gap_2()
                .items_center()
                .overflow_hidden()
                .child(Skeleton::new().size_3p5().rounded(px(3.)).flex_shrink_0())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Skeleton::new().w(relative(width)).h(px(10.))),
                )
        }))
        .into_any_element()
}

/// The item header, facets, merge card, body, and fixed comment composer.
pub fn detail(cx: &App) -> AnyElement {
    let tokens = Tokens::global(cx);
    v_flex()
        .size_full()
        .min_w_0()
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .px_5()
                .pt_4()
                .pb_3()
                .gap_2()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(bar(px(34.), px(11.), cx).secondary())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Skeleton::new().w(relative(0.72)).h(px(14.))),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .overflow_hidden()
                        .child(bar(px(56.), px(20.), cx))
                        .child(Skeleton::new().size(px(18.)).rounded_full())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Skeleton::new().w(relative(0.78)).h(px(10.)).secondary()),
                        )
                        .child(bar(px(76.), px(18.), cx).secondary()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(bar(px(58.), px(24.), cx).secondary())
                        .child(bar(px(96.), px(24.), cx).secondary()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(bar(px(92.), px(22.), cx).secondary())
                        .child(bar(px(62.), px(22.), cx).secondary()),
                ),
        )
        .child(
            v_flex()
                .id("detail-skeleton-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .px_5()
                .py_4()
                .gap_4()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(720.))
                        .gap_3()
                        .children((0..3).map(|index| {
                            h_flex()
                                .w_full()
                                .gap_3()
                                .items_center()
                                .child(bar(px(72.), px(10.), cx).secondary())
                                .child(bar(px([84., 112., 96.][index]), px(15.), cx))
                        })),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(720.))
                        .h_px()
                        .bg(tokens.colors().border_subtle),
                )
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(720.))
                        .p_3()
                        .gap_3()
                        .border_1()
                        .border_color(tokens.colors().border_subtle)
                        .rounded(px(tokens.radius.panel))
                        .child(Skeleton::new().w(relative(0.56)).h(px(12.)))
                        .child(Skeleton::new().w(relative(0.78)).h(px(10.)).secondary())
                        .child(Skeleton::new().w(relative(0.54)).h(px(10.)).secondary())
                        .child(Skeleton::new().w(relative(0.42)).h(px(30.))),
                )
                .child(v_flex().w_full().max_w(px(720.)).gap_2().children(
                    [0.88, 0.94, 0.56, 0.78].into_iter().map(|fraction| {
                        Skeleton::new()
                            .w(relative(fraction))
                            .h(px(11.))
                            .rounded(px(3.))
                            .secondary()
                    }),
                ))
                .child(
                    h_flex()
                        .w_full()
                        .max_w(px(720.))
                        .gap_2()
                        .items_start()
                        .child(Skeleton::new().size(px(20.)).rounded_full())
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(Skeleton::new().w(relative(0.34)).h(px(10.)))
                                .child(Skeleton::new().w(relative(0.72)).h(px(10.)).secondary()),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .px_5()
                .pt_2()
                .pb_4()
                .border_t_1()
                .border_color(tokens.colors().border_subtle)
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .max_w(px(720.))
                        .p_2()
                        .gap_2()
                        .rounded(px(tokens.radius.control() + 2.))
                        .border_1()
                        .border_color(tokens.colors().border_subtle)
                        .child(Skeleton::new().w(relative(0.46)).h(px(11.)).secondary())
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .flex_wrap()
                                .child(bar(px(76.), px(25.), cx).secondary())
                                .child(bar(px(76.), px(25.), cx).secondary())
                                .child(bar(px(84.), px(25.), cx)),
                        ),
                ),
        )
        .into_any_element()
}

/// The shape of a diff: file status and counts, followed by numbered lines.
pub fn diff(cx: &App) -> AnyElement {
    let tokens = Tokens::global(cx);
    v_flex()
        .w_full()
        .py_1()
        .children((0..2).flat_map(|file| {
            std::iter::once(
                h_flex()
                    .w_full()
                    .h(px(22.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .bg(tokens.colors().bg_raised)
                    .child(bar(px(12.), px(10.), cx).secondary())
                    .child(bar(px(12.), px(10.), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Skeleton::new().w(relative([0.72, 0.55][file])).h(px(10.))),
                    )
                    .child(bar(px(26.), px(9.), cx).secondary())
                    .child(bar(px(26.), px(9.), cx).secondary())
                    .into_any_element(),
            )
            .chain((0..6).map(|line| {
                h_flex()
                    .w_full()
                    .h(px(22.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .child(bar(px(28.), px(8.), cx).secondary())
                    .child(bar(px(28.), px(8.), cx).secondary())
                    .child(bar(px(8.), px(8.), cx).secondary())
                    .child(
                        div().flex_1().min_w_0().child(
                            Skeleton::new()
                                .w(relative([0.52, 0.7, 0.32, 0.62, 0.46, 0.66][line]))
                                .h(px(9.))
                                .secondary(),
                        ),
                    )
                    .into_any_element()
            }))
        }))
        .into_any_element()
}
