//! The session sidebar: `docs/ui.md` §3.2.
//!
//! Three lines per row — origin + status, title, worktree branch — with an
//! archived section below. M0 draws the structure against sample rows; M1
//! swaps in real data and makes the list virtualized.

use ginka_ui::Tokens;
use ginka_ui::workspace::{AgentState, SessionRow};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};

pub struct SessionSidebar {
    rows: Vec<SessionRow>,
    selected: usize,
    archived_open: bool,
}

impl SessionSidebar {
    pub fn new(rows: Vec<SessionRow>) -> Self {
        Self {
            rows,
            selected: 0,
            archived_open: true,
        }
    }

    fn header(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2p5()
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::Folder)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                h_flex()
                    .flex_1()
                    .gap_1()
                    .items_baseline()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(tokens.colors().text_primary)
                            .child("ginka"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child("@ personal-metal"),
                    ),
            )
            .child(
                Icon::new(IconName::ChevronDown)
                    .size_3()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                Icon::new(IconName::Plus)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
    }

    /// The status pill, or the relative time when nothing is running.
    fn status(&self, row: &SessionRow, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let color = match row.state {
            AgentState::Working => tokens.colors().status_working,
            AgentState::NeedsAttention => tokens.colors().status_attention,
            AgentState::Idle => tokens.colors().text_muted,
        };

        h_flex()
            .gap_1p5()
            .items_center()
            .when_some(row.state.label(), |this, label| {
                this.child(div().size_1p5().rounded_full().bg(color))
                    .child(div().text_xs().text_color(color).child(label))
            })
            .when(row.state.label().is_none(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(row.age.clone()),
                )
            })
    }

    fn row(&self, index: usize, row: &SessionRow, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let selected = index == self.selected;

        v_flex()
            .id(("session", index))
            .w_full()
            .px_2p5()
            .py_2()
            .gap_0p5()
            .rounded(px(tokens.radius.row))
            .when(selected, |this| this.bg(tokens.colors().bg_raised))
            .hover(|this| this.bg(tokens.colors().bg_raised.opacity(0.6)))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(row.origin.clone()),
                    )
                    .child(self.status(row, cx)),
            )
            .child(
                div()
                    .w_full()
                    .text_sm()
                    .font_medium()
                    .text_color(tokens.colors().text_primary)
                    .truncate()
                    .child(row.title.clone()),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .items_center()
                    .child(
                        Icon::empty()
                            .path(ginka_ui::assets::icon::GIT_BRANCH)
                            .size_3()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(tokens.colors().text_secondary)
                            // Paths and branches carry their meaning in the
                            // tail, so the head is what gets dropped.
                            .truncate()
                            .child(row.branch.clone()),
                    ),
            )
    }

    fn archived_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .id("archived-header")
            .w_full()
            .px_2p5()
            .py_1p5()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child("Archived"),
            )
            .child(
                Icon::new(if self.archived_open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .text_color(tokens.colors().text_muted),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.archived_open = !this.archived_open;
                cx.notify();
            }))
    }

    fn archived_row(&self, index: usize, row: &SessionRow, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .id(("archived", index))
            .w_full()
            .px_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .rounded(px(tokens.radius.row))
            .hover(|this| this.bg(tokens.colors().bg_raised.opacity(0.6)))
            .child(
                Icon::new(IconName::Inbox)
                    .size_3p5()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(tokens.colors().text_secondary)
                    .truncate()
                    .child(row.title.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(row.age.clone()),
            )
    }

    fn footer(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2p5()
            .gap_2p5()
            .items_center()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                div()
                    .size_7()
                    .rounded_full()
                    .bg(tokens.colors().accent.opacity(0.25))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(tokens.colors().text_primary)
                    .child("G"),
            )
            .child(
                v_flex()
                    .flex_1()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(tokens.colors().text_primary)
                            .child("Local"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child("M0"),
                    ),
            )
    }
}

impl Render for SessionSidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let border = tokens.colors().border_subtle;
        let sidebar_bg = tokens.colors().bg_sidebar;

        let mut active: Vec<(usize, SessionRow)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.archived)
            .map(|(index, row)| (index, row.clone()))
            .collect();
        // Stable: equal ranks keep their insertion order.
        active.sort_by_key(|(_, row)| row.attention_rank());

        let archived: Vec<(usize, SessionRow)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.archived)
            .map(|(index, row)| (index, row.clone()))
            .collect();
        let archived_count = archived.len();

        v_flex()
            .size_full()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(border)
            .child(self.header(cx))
            .child(
                v_flex()
                    .id("session-list")
                    .flex_1()
                    .px_1p5()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .children(
                        active
                            .iter()
                            .map(|(index, row)| self.row(*index, row, cx).into_any_element()),
                    )
                    .child(div().h_2())
                    .child(self.archived_header(cx))
                    .when(self.archived_open, |this| {
                        this.children(archived.iter().map(|(index, row)| {
                            self.archived_row(*index, row, cx).into_any_element()
                        }))
                    })
                    .when(self.archived_open && archived_count > 0, |this| {
                        this.child(
                            div()
                                .px_2p5()
                                .py_1p5()
                                .text_xs()
                                .text_color(Tokens::global(cx).colors().text_muted)
                                .child("Show more"),
                        )
                    }),
            )
            .child(self.footer(cx))
    }
}
