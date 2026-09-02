//! The session sidebar: `docs/ui.md` §3.2.
//!
//! Workspaces under the project they belong to, the way a file tree puts files
//! under a folder. The project was a line on every row before, which is a line
//! per row spent repeating what the heading above it already says, and it left
//! a reader scanning for "which project is this" with nowhere single to look.
//!
//! A row is its title and what is happening to it. Its branch only earns a
//! second line when it says something the title does not — which is when an
//! agent has checked out something else inside the worktree.

use ginka_ui::Tokens;
use ginka_ui::workspace::{AgentState, SessionRow, group_by_project};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};

/// Emitted when the user picks a row.
///
/// No payload: the shell reads the selection back through `selected_row`, so
/// there is one place that decides what "selected" means. The sidebar does not
/// know what the centre column does with it, and the shell does not know how
/// the sidebar is drawn.
pub enum SidebarEvent {
    Selected,
}

impl EventEmitter<SidebarEvent> for SessionSidebar {}

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

    /// Replace the rows after a refresh.
    ///
    /// The selection follows the *workspace*, not the index: rows are sorted by
    /// attention, so an index means nothing across a reload and keeping one
    /// would move the user's selection whenever an agent started somewhere else.
    ///
    /// By workspace id rather than by branch, because the branch is the live
    /// one: an agent that checks out something else inside the worktree would
    /// otherwise move the selection out from under the user mid-answer.
    pub fn set_rows(&mut self, rows: Vec<SessionRow>, cx: &mut Context<Self>) {
        let selected = self
            .rows
            .get(self.selected)
            .map(|row| row.workspace.clone());
        self.selected = selected
            .and_then(|workspace| rows.iter().position(|row| row.workspace == workspace))
            .unwrap_or(0);
        self.rows = rows;
        cx.notify();
    }

    pub fn selected_row(&self) -> Option<&SessionRow> {
        self.rows.get(self.selected)
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected == index {
            return;
        }
        self.selected = index;
        cx.emit(SidebarEvent::Selected);
        cx.notify();
    }

    /// The app's own header: what this is, and the two things a header is
    /// for — finding a workspace, and making one.
    fn header(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2p5()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(tokens.colors().text_primary)
                    .child("ginka"),
            )
            .child(
                Icon::new(IconName::ChevronDown)
                    .size_3()
                    .text_color(tokens.colors().text_muted),
            )
            .child(div().flex_1())
            .child(
                Icon::new(IconName::Search)
                    .size_4()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                Icon::new(IconName::Plus)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
    }

    /// A small muted label over a run of rows.
    fn section(&self, label: String, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        div()
            .w_full()
            .px_3()
            .pt_3()
            .pb_1()
            .text_xs()
            .text_color(tokens.colors().text_muted)
            .child(label)
    }

    /// The heading a project's workspaces hang under.
    fn project_header(&self, project: SharedString, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::Folder)
                    .size_3p5()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(tokens.colors().text_secondary)
                    .truncate()
                    .child(project),
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

    fn session_row(
        &self,
        index: usize,
        row: &SessionRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let selected = index == self.selected;
        let branch = row.branch_worth_showing();

        v_flex()
            .id(("session", index))
            .w_full()
            // Indented under the project heading: the indent is what says
            // these belong to it.
            .ml_3()
            .px_2p5()
            .py_1p5()
            .gap_0p5()
            .rounded(px(tokens.radius.row))
            .when(selected, |this| this.bg(tokens.colors().bg_raised))
            .hover(|this| this.bg(tokens.colors().bg_raised.opacity(0.55)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(row.agent.glyph().size_3p5().text_color(if selected {
                        tokens.colors().text_secondary
                    } else {
                        tokens.colors().text_muted
                    }))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .when(selected, |this| this.font_medium())
                            .text_color(if selected {
                                tokens.colors().text_primary
                            } else {
                                tokens.colors().text_secondary
                            })
                            .truncate()
                            .child(row.title.clone()),
                    )
                    .child(self.status(row, cx)),
            )
            .when(branch || row.status.dirty || row.status.conflict, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_1p5()
                        .items_center()
                        .when(branch, |this| {
                            this.child(
                                Icon::empty()
                                    .path(ginka_ui::assets::icon::GIT_BRANCH)
                                    .size_3()
                                    .text_color(tokens.colors().text_muted),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    // Paths and branches carry their meaning in
                                    // the tail, so the head is what gets
                                    // dropped.
                                    .truncate()
                                    .child(row.branch.clone()),
                            )
                        })
                        .when(!branch, |this| this.child(div().flex_1()))
                        .when(row.status.conflict, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.colors().status_error)
                                    .child("!"),
                            )
                        })
                        .when(row.status.dirty && !row.status.conflict, |this| {
                            // An unlabelled dot: the sidebar has no room for
                            // the word, and "there are changes here" is the
                            // whole message.
                            this.child(
                                div()
                                    .size_1p5()
                                    .rounded_full()
                                    .bg(tokens.colors().status_attention),
                            )
                        })
                        .children(row.divergence().map(|summary| {
                            div()
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .child(summary)
                        })),
                )
            })
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
                    .child(rust_i18n::t!("sidebar.archived").to_string()),
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

    fn archived_row(
        &self,
        index: usize,
        row: &SessionRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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

    /// First run: nothing is registered yet.
    ///
    /// Says how to fix it rather than only that the list is empty. The command
    /// is the real one, so it can be copied straight into a terminal.
    fn empty_state(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .px_4()
            .gap_2()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.colors().text_secondary)
                    .child(rust_i18n::t!("sidebar.empty.title").to_string()),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(px(tokens.radius.row))
                    .bg(tokens.colors().code_bg)
                    .text_xs()
                    .text_color(tokens.colors().text_secondary)
                    .child(rust_i18n::t!("sidebar.empty.hint").to_string()),
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
                            .child(rust_i18n::t!("sidebar.footer.device").to_string()),
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
        // Grouped under their projects, which is also what orders the projects:
        // the one with an agent working in it rises the way a row does.
        let active_rows: Vec<SessionRow> = active.iter().map(|(_, row)| row.clone()).collect();
        let groups = group_by_project(&active_rows)
            .into_iter()
            .map(|mut group| {
                // `group_by_project` numbers rows within what it was given;
                // selection is addressed by the index in `self.rows`.
                group.rows = group
                    .rows
                    .into_iter()
                    .map(|(within, row)| (active[within].0, row))
                    .collect();
                group
            })
            .collect::<Vec<_>>();

        let archived: Vec<(usize, SessionRow)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.archived)
            .map(|(index, row)| (index, row.clone()))
            .collect();
        let archived_count = archived.len();

        let is_empty = self.rows.is_empty();

        v_flex()
            .size_full()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(border)
            .child(self.header(cx))
            .when(is_empty, |this| this.child(self.empty_state(cx)))
            .when(!is_empty, |this| {
                this.child(
                    v_flex()
                        .id("session-list")
                        .flex_1()
                        .px_1p5()
                        .gap_0p5()
                        .overflow_y_scroll()
                        .child(self.section(rust_i18n::t!("sidebar.projects").to_string(), cx))
                        .children(groups.into_iter().flat_map(|group| {
                            let mut elements =
                                vec![self.project_header(group.project, cx).into_any_element()];
                            elements.extend(group.rows.iter().map(|(index, row)| {
                                self.session_row(*index, row, cx).into_any_element()
                            }));
                            elements
                        }))
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
                                    // The count is a placeholder until the
                                    // archived list is paged (M1).
                                    .child(
                                        rust_i18n::t!("sidebar.show_more", count = 25).to_string(),
                                    ),
                            )
                        }),
                )
            })
            .child(self.footer(cx))
    }
}
