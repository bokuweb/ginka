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
//!
//! The headings are themselves selectable, because a project is something the
//! reader picks *before* there is a conversation: choosing one and then "new
//! chat" is how a chat is started in it, and a heading that could only be read
//! would leave that flow with no way in.

use ginka_protocol::{ProjectName, WorkspaceId};
use ginka_ui::Tokens;
use ginka_ui::workspace::{AgentState, ProjectRow, SessionRow, tree};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputState};
use gpui_component::{
    Icon, IconName, StyledExt as _, h_flex, scroll::ScrollableElement as _, v_flex,
};

/// Emitted when the user picks a row.
///
/// No payload: the shell reads the selection back through `selected_row`, so
/// there is one place that decides what "selected" means. The sidebar does not
/// know what the centre column does with it, and the shell does not know how
/// the sidebar is drawn.
pub enum SidebarEvent {
    Selected,
    /// A project was picked, and no workspace under it. The centre column has
    /// no conversation to show for that, which is exactly the point: it shows
    /// the home screen, aimed at this project.
    ProjectSelected,
    /// The reader asked for a project to be registered. The sidebar does not
    /// know how to pick a folder or how to reach the daemon; the shell does.
    AddProjectRequested,
    /// The reader asked to start a new conversation. Where it runs is whatever
    /// is selected — a project, or the project of the selected workspace, or
    /// nothing at all, which is a scratch worktree. What that costs is the
    /// shell's to decide.
    NewChatRequested,
}

impl EventEmitter<SidebarEvent> for SessionSidebar {}

pub struct SessionSidebar {
    /// Every registered project, in the daemon's order.
    projects: Vec<ProjectRow>,
    rows: Vec<SessionRow>,
    /// The workspace the centre column is showing, if it is showing one.
    ///
    /// By id rather than by index: rows are sorted by attention, so an index
    /// means nothing across a reload and keeping one would move the user's
    /// selection whenever an agent started somewhere else. By workspace rather
    /// than by branch, because the branch is the live one — an agent that
    /// checks out something else inside the worktree must not move the
    /// selection out from under the reader mid-answer.
    selected: Option<WorkspaceId>,
    /// Which project the next chat belongs to. Set by picking a heading, and
    /// by picking a row under one.
    selected_project: Option<ProjectName>,
    /// How many refreshes in a row have not listed the selected workspace.
    /// See [`SessionSidebar::set_rows`].
    unlisted: u8,
    archived_open: bool,
    /// The login the next prompt runs on, and its headroom, for the footer
    /// (`docs/accounts.md` §11). The shell decides both; the sidebar draws
    /// them.
    account: Option<(String, Option<String>)>,
    /// The session-list search field, owned by the shell so its events can
    /// update this view without putting decision logic in the binary crate.
    search: Entity<InputState>,
    search_query: String,
}

impl SessionSidebar {
    pub fn new(rows: Vec<SessionRow>, search: Entity<InputState>) -> Self {
        Self {
            projects: Vec::new(),
            rows,
            selected: None,
            selected_project: None,
            unlisted: 0,
            archived_open: true,
            account: None,
            search,
            search_query: String::new(),
        }
    }

    /// Apply the current conversation query from the search input.
    pub fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        if self.search_query != query {
            self.search_query = query;
            cx.notify();
        }
    }

    /// Say which login the next prompt runs on, and how much of its window
    /// is left, for the footer. `None` when nothing is known yet.
    pub fn set_account(
        &mut self,
        account: Option<(String, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        if self.account != account {
            self.account = account;
            cx.notify();
        }
    }

    /// Replace the rows after a refresh.
    ///
    /// A selection whose workspace is no longer listed is dropped, but only
    /// once a second refresh agrees it is gone: a workspace this window has
    /// just created is selected before the listing that first mentions it, and
    /// dropping it there would take the reader out of the chat they just
    /// started. Two refreshes in a row without it means the worktree was
    /// removed, and the window falls back to the home screen rather than to
    /// whatever row is now first — that would be a conversation nobody asked
    /// for.
    pub fn set_rows(&mut self, rows: Vec<SessionRow>, cx: &mut Context<Self>) {
        self.rows = rows;
        match &self.selected {
            Some(selected) if !self.rows.iter().any(|row| &row.workspace == selected) => {
                self.unlisted += 1;
                if self.unlisted > 1 {
                    self.selected = None;
                    self.unlisted = 0;
                }
            }
            _ => self.unlisted = 0,
        }
        cx.notify();
    }

    /// Replace the projects after a refresh.
    pub fn set_projects(&mut self, projects: Vec<ProjectRow>, cx: &mut Context<Self>) {
        self.projects = projects;
        cx.notify();
    }

    pub fn selected_row(&self) -> Option<&SessionRow> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().find(|row| &row.workspace == selected)
    }

    /// The workspace the centre column is showing, whether or not a refresh
    /// has listed it yet.
    pub fn selection(&self) -> Option<&WorkspaceId> {
        self.selected.as_ref()
    }

    /// Which project a new chat would belong to.
    pub fn selected_project(&self) -> Option<&ProjectName> {
        self.selected_project.as_ref()
    }

    /// Whether the contextual sessions column should be visible.
    pub fn has_project_selection(&self) -> bool {
        self.selected_project.is_some()
    }

    /// The rows as they stand, for the palette to offer.
    pub fn rows(&self) -> &[SessionRow] {
        &self.rows
    }

    /// Select a workspace by name, which is how the palette switches to one.
    pub fn select_workspace(&mut self, workspace: &WorkspaceId, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(workspace) {
            return;
        }
        self.adopt_workspace(workspace.clone(), cx);
        cx.emit(SidebarEvent::Selected);
    }

    /// Move the selection without announcing it.
    ///
    /// For the shell adopting a workspace it has just started a chat in: the
    /// announcement is what clears the transcript, and clearing the one that
    /// was just created is not what "select this" meant here.
    pub fn adopt_workspace(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        self.selected_project = self
            .rows
            .iter()
            .find(|row| row.workspace == workspace)
            .map(|row| ProjectName(row.origin.to_string()))
            .or_else(|| {
                // A workspace id is `<project>/<name>`, so the project is
                // known even before the refresh that lists the row.
                workspace
                    .0
                    .split_once('/')
                    .map(|(project, _)| ProjectName(project.to_string()))
            });
        self.selected = Some(workspace);
        self.unlisted = 0;
        cx.notify();
    }

    /// Pick a project, and nothing in it.
    pub fn select_project(&mut self, project: ProjectName, cx: &mut Context<Self>) {
        self.aim_at(Some(project), cx);
        cx.emit(SidebarEvent::ProjectSelected);
    }

    /// Aim the next chat at a project — or at none — without announcing it.
    ///
    /// For the shell, which is where the decision came from: announcing it
    /// back would have the shell answer its own message.
    pub fn aim_at(&mut self, project: Option<ProjectName>, cx: &mut Context<Self>) {
        self.selected = None;
        self.unlisted = 0;
        self.selected_project = project;
        cx.notify();
    }

    /// The app's own header: what this is, and the two things a header is
    /// for — finding a workspace, and making one.
    /// The front door for a conversation.
    ///
    /// Above the list rather than beside the composer: starting a new one is
    /// the first thing a reader does with this window, and a control they have
    /// to find inside the last conversation is a control they do not find.
    fn new_chat(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        h_flex()
            .id("new-chat")
            .w_full()
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .child(
                Icon::empty()
                    .path(ginka_ui::assets::icon::SQUARE_PEN)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.colors().text_primary)
                    .child(rust_i18n::t!("sidebar.new_chat").to_string()),
            )
            .on_click(cx.listener(|_, _, _, cx| {
                cx.emit(SidebarEvent::NewChatRequested);
            }))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
                div()
                    .id("focus-session-search")
                    .p_1()
                    .rounded(px(tokens.radius.control()))
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .child(
                        Icon::new(IconName::Search)
                            .size_4()
                            .text_color(tokens.colors().text_muted),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.search.read(cx).focus_handle(cx).focus(window, cx);
                    })),
            )
            .child(
                div()
                    .id("header-new-chat")
                    .cursor_pointer()
                    .child(
                        Icon::new(IconName::Plus)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SidebarEvent::NewChatRequested);
                    })),
            )
    }

    /// A small muted label over a run of rows.
    fn section(&self, label: String, cx: &App) -> impl IntoElement + use<> {
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

    /// The `Projects` label, with the way to add one beside it.
    ///
    /// Next to the heading rather than only in the empty state: a reader with
    /// one project registered wants the second one added from the same place,
    /// and an empty state is by definition not there any more once they do.
    fn projects_section(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .child(self.section(rust_i18n::t!("sidebar.projects").to_string(), cx)),
            )
            .child(
                div()
                    .id("add-project")
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconName::Plus)
                            .size_3p5()
                            .text_color(tokens.colors().text_muted),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SidebarEvent::AddProjectRequested);
                    })),
            )
    }

    /// The heading a project's workspaces hang under, and the way to aim a new
    /// chat at it.
    fn project_header(
        &self,
        project: ProjectName,
        label: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        // The project rail and the session list are separate navigation
        // levels, so both stay selected while a conversation is open.
        let selected = self.selected_project.as_ref() == Some(&project);
        h_flex()
            .id(SharedString::from(format!("project:{}", project.0)))
            .w_full()
            .px_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .when(selected, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.select_project(project.clone(), cx)))
            .child(
                Icon::new(IconName::Folder)
                    .size_3p5()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(if selected {
                        tokens.colors().text_primary
                    } else {
                        tokens.colors().text_secondary
                    })
                    .truncate()
                    .child(label),
            )
    }

    /// Header for the session list beside the project rail.
    fn workspace_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        h_flex()
            .w_full()
            .h(px(44.))
            .px_3()
            .items_center()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_semibold()
                    .text_color(tokens.colors().text_primary)
                    .child(rust_i18n::t!("sidebar.workspace").to_string()),
            )
            .child(
                div()
                    .id("workspace-new-chat")
                    .p_1()
                    .rounded(px(tokens.radius.control()))
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .child(
                        Icon::new(IconName::Plus)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SidebarEvent::NewChatRequested);
                    })),
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
        let selected = self.selected.as_ref() == Some(&row.workspace);
        let branch = row.branch_worth_showing();
        let workspace = row.workspace.clone();

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
            .when(selected, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.select_workspace(&workspace, cx)))
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
            .hover(|this| this.bg(tokens.colors().row_hover()))
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
    /// A quiet line under the section heading. The adjacent plus is the one
    /// way into the modal, so this state does not repeat it as another button.
    fn no_projects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        div()
            .w_full()
            .px_3()
            .py_1p5()
            .text_xs()
            .text_color(tokens.colors().text_muted)
            .child(rust_i18n::t!("sidebar.empty.title").to_string())
    }

    /// The plan label `docs/ui.md` §3.2 reserves: the login in use, and its
    /// headroom under it. The device line stands in until a login is known.
    fn footer(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let (title, detail) = match &self.account {
            Some((label, headroom)) => (
                label.clone(),
                headroom
                    .clone()
                    .unwrap_or_else(|| rust_i18n::t!("sidebar.footer.device").to_string()),
            ),
            None => (
                rust_i18n::t!("sidebar.footer.device").to_string(),
                String::new(),
            ),
        };
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
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(detail),
                    ),
            )
    }
}

impl Render for SessionSidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let border = tokens.colors().border_subtle;
        let sidebar_bg = tokens.colors().bg_sidebar;

        let mut active: Vec<(usize, SessionRow)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.archived)
            .filter(|(_, row)| ginka_ui::workspace::session_matches(row, &self.search_query))
            .map(|(index, row)| (index, row.clone()))
            .collect();
        // Stable: equal ranks keep their insertion order.
        active.sort_by_key(|(_, row)| row.attention_rank());
        // Grouped under their projects, which is also what orders the projects:
        // the one with an agent working in it rises the way a row does.
        let active_rows: Vec<SessionRow> = active.iter().map(|(_, row)| row.clone()).collect();
        let mut groups = tree(&self.projects, &active_rows)
            .into_iter()
            .map(|mut group| {
                // `tree` numbers rows within what it was given; a row element
                // is keyed by the index it has in the flat list.
                group.rows = group
                    .rows
                    .into_iter()
                    .map(|(within, row)| (active[within].0, row))
                    .collect();
                group
            })
            .collect::<Vec<_>>();
        if let Some(project) = &self.selected_project {
            groups.retain(|group| &group.name == project);
        }

        let selected_project_name = self.selected_project.clone();
        let archived: Vec<(usize, SessionRow)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.archived)
            .filter(|(_, row)| ginka_ui::workspace::session_matches(row, &self.search_query))
            .filter(|(_, row)| {
                selected_project_name.as_ref().is_none_or(|selected| {
                    row.workspace
                        .parts()
                        .is_some_and(|(project, _)| &project == selected)
                })
            })
            .map(|(index, row)| (index, row.clone()))
            .collect();
        let archived_count = archived.len();
        let no_matching_sessions =
            groups.iter().all(|group| group.rows.is_empty()) && archived_count == 0;

        let project_rows = self.projects.clone();
        let selected_project = selected_project_name.is_some();

        h_flex()
            .size_full()
            .border_r_1()
            .border_color(border)
            .child(
                v_flex()
                    .w(px(188.))
                    .h_full()
                    .flex_shrink_0()
                    .bg(sidebar_bg)
                    .border_r_1()
                    .border_color(border)
                    .child(self.header(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .px_1p5()
                            .gap_0p5()
                            .overflow_y_scrollbar()
                            .child(self.projects_section(cx))
                            .when(project_rows.is_empty(), |this| {
                                this.child(self.no_projects(cx))
                            })
                            .children(project_rows.into_iter().map(|project| {
                                self.project_header(project.name, project.label, cx)
                            })),
                    ),
            )
            .when(selected_project, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .bg(tokens.colors().bg_window.opacity(0.34))
                        .child(self.workspace_header(cx))
                        .child(
                            h_flex()
                                .h(px(36.))
                                .mx_2()
                                .mt_2()
                                .px_2()
                                .gap_2()
                                .items_center()
                                .rounded(px(tokens.radius.control()))
                                .bg(tokens.colors().row_hover())
                                .child(
                                    Icon::new(IconName::Search)
                                        .size_3p5()
                                        .text_color(tokens.colors().text_muted),
                                )
                                .child(div().flex_1().min_w_0().child(Input::new(&self.search)))
                                .when(!self.search_query.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .id("clear-session-search")
                                            .p_1()
                                            .rounded(px(tokens.radius.control()))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(tokens.colors().row_active()))
                                            .child(
                                                Icon::new(IconName::Close)
                                                    .size_3()
                                                    .text_color(tokens.colors().text_muted),
                                            )
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.search.update(cx, |search, cx| {
                                                    search.set_value("", window, cx)
                                                });
                                            })),
                                    )
                                }),
                        )
                        .child(self.new_chat(cx))
                        .child(
                            v_flex()
                                .id("session-list")
                                .flex_1()
                                .min_h_0()
                                .px_1p5()
                                .gap_0p5()
                                .overflow_y_scroll()
                                .when(no_matching_sessions, |this| {
                                    this.child(
                                        div()
                                            .px_3()
                                            .py_2()
                                            .text_xs()
                                            .text_color(tokens.colors().text_muted)
                                            .child(if self.search_query.trim().is_empty() {
                                                rust_i18n::t!("sidebar.sessions.empty").to_string()
                                            } else {
                                                rust_i18n::t!("sidebar.sessions.no_match")
                                                    .to_string()
                                            }),
                                    )
                                })
                                .children(groups.into_iter().flat_map(|group| {
                                    let mut elements = Vec::new();
                                    if !selected_project {
                                        elements.push(
                                            self.project_header(group.name, group.project, cx)
                                                .into_any_element(),
                                        );
                                    }
                                    elements.extend(group.rows.iter().map(|(index, row)| {
                                        self.session_row(*index, row, cx).into_any_element()
                                    }));
                                    elements
                                }))
                                .when(archived_count > 0, |this| {
                                    this.child(div().h_2())
                                        .child(self.archived_header(cx))
                                        .when(self.archived_open, |this| {
                                            this.children(archived.iter().map(|(index, row)| {
                                                self.archived_row(*index, row, cx)
                                                    .into_any_element()
                                            }))
                                            .child(
                                                div()
                                                    .px_2p5()
                                                    .py_1p5()
                                                    .text_xs()
                                                    .text_color(tokens.colors().text_muted)
                                                    .child(
                                                        rust_i18n::t!(
                                                            "sidebar.show_more",
                                                            count = 25
                                                        )
                                                        .to_string(),
                                                    ),
                                            )
                                        })
                                }),
                        )
                        .child(self.footer(cx)),
                )
            })
    }
}
