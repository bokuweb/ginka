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

use ginka_protocol::{ProjectName, SessionId, WorkspaceId};
use ginka_ui::Tokens;
use ginka_ui::workspace::{AgentState, ProjectRow, SessionRow, session_shortcuts, tree};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
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
    /// The reader picked one of the places beside the projects: the inbox,
    /// the notes, the settings — or a project again.
    Open(Place),
    /// Keep a workspace at the top of its project's list, or stop.
    Pin {
        workspace: WorkspaceId,
        pinned: bool,
    },
    /// Move a workspace to the archived section, or bring it back.
    Archive {
        workspace: WorkspaceId,
        archived: bool,
    },
    /// Give a conversation a title of the reader's own.
    Rename {
        session: SessionId,
        title: String,
    },
    /// Show a project under another name; blank goes back to its own.
    LabelProject {
        project: ProjectName,
        label: String,
    },
    /// Put a project at another place in the rail.
    MoveProject {
        project: ProjectName,
        index: u32,
    },
}

/// Where the window is, as the project rail offers it (`docs/ui.md` §3.2).
///
/// A project is a place; the inbox, the notes and the settings are the
/// others, each bringing a column of its own. MonoCode's navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A project's sessions and conversations.
    Workspace,
    /// Pull requests and issues, from the GitHub client.
    Inbox,
    /// The reader's markdown notes.
    Notes,
    /// Appearance and language.
    Settings,
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
    /// The session list, virtualized (`AGENTS.md` rule 5): only the rows
    /// near the screen are laid out, however many sessions there are.
    session_list: ListState,
    /// What the list shows, one entry per item, as of the last frame.
    entries: Vec<ginka_ui::session_list::Entry>,
    /// The login the next prompt runs on, and its headroom, for the footer
    /// (`docs/accounts.md` §11). The shell decides both; the sidebar draws
    /// them.
    account: Option<(String, Option<String>)>,
    /// The session-list search field, owned by the shell so its events can
    /// update this view without putting decision logic in the binary crate.
    search: Entity<InputState>,
    search_query: String,
    /// Whether a locally picked project path names the daemon's filesystem.
    local_paths: bool,
    /// Which place the window is showing.
    place: Place,
    /// Whether the inbox has something unread. Known only once the GitHub
    /// client has been opened; the dot is never a guess.
    inbox_unread: bool,
    /// Which sessions are listed, by what they are doing.
    status: ginka_ui::workspace::StatusFilter,
    /// The row whose actions are open: MonoCode's session menu, drawn under
    /// the row rather than floating, so the list stays one column.
    menu_for: Option<WorkspaceId>,
    /// The project whose menu is open in the rail.
    project_menu_for: Option<ProjectName>,
    /// The project being renamed, and the field its name is typed in.
    labelling: Option<(ProjectName, Entity<InputState>)>,
    /// The conversation being renamed, and the field its title is typed in.
    renaming: Option<(WorkspaceId, SessionId, Entity<InputState>)>,
}

impl SessionSidebar {
    pub fn new(rows: Vec<SessionRow>, search: Entity<InputState>, local_paths: bool) -> Self {
        Self {
            projects: Vec::new(),
            rows,
            selected: None,
            selected_project: None,
            unlisted: 0,
            archived_open: true,
            session_list: ListState::new(0, ListAlignment::Top, px(600.)),
            entries: Vec::new(),
            account: None,
            search,
            search_query: String::new(),
            local_paths,
            place: Place::Workspace,
            inbox_unread: false,
            status: ginka_ui::workspace::StatusFilter::All,
            menu_for: None,
            project_menu_for: None,
            labelling: None,
            renaming: None,
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
        // A row's title or state can change its height where the list is not
        // looking; the rows on screen are laid out again anyway.
        self.session_list.remeasure();
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
        self.selected_project.is_some() && self.place == Place::Workspace
    }

    /// Which place the window is showing.
    pub fn place(&self) -> Place {
        self.place
    }

    /// Say which place the window is showing, without announcing it.
    pub fn set_place(&mut self, place: Place, cx: &mut Context<Self>) {
        if self.place != place {
            self.place = place;
            cx.notify();
        }
    }

    /// Say whether the inbox has anything unread, for the dot beside it.
    #[cfg_attr(not(feature = "github"), allow(dead_code))]
    pub fn set_inbox_unread(&mut self, unread: bool, cx: &mut Context<Self>) {
        if self.inbox_unread != unread {
            self.inbox_unread = unread;
            cx.notify();
        }
    }

    /// Go to a place, and tell the shell.
    fn open(&mut self, place: Place, cx: &mut Context<Self>) {
        self.set_place(place, cx);
        cx.emit(SidebarEvent::Open(place));
    }

    /// One of the fixed places in the rail: an icon and a word, lit while it
    /// is the one on screen.
    fn place_row(
        &self,
        place: Place,
        icon: Icon,
        label: String,
        hint: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        let on = self.place == place;
        h_flex()
            .id(SharedString::from(format!("place:{place:?}")))
            .w_full()
            .px_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .when(on, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.open(place, cx)))
            .child(icon.size_3p5().text_color(if on {
                tokens.colors().text_primary
            } else {
                tokens.colors().text_muted
            }))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(if on {
                        tokens.colors().text_primary
                    } else {
                        tokens.colors().text_secondary
                    })
                    .child(label),
            )
            .when(place == Place::Inbox && self.inbox_unread, |this| {
                this.child(div().size(px(7.)).rounded_full().bg(tokens.colors().accent))
            })
            .children(hint.map(|hint| {
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(hint)
            }))
    }

    /// The rows as they stand, for the palette to offer.
    pub fn rows(&self) -> &[SessionRow] {
        &self.rows
    }

    /// Select a workspace by name, which is how the palette switches to one.
    pub fn select_workspace(&mut self, workspace: &WorkspaceId, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(workspace) && self.place == Place::Workspace {
            return;
        }
        self.place = Place::Workspace;
        self.adopt_workspace(workspace.clone(), cx);
        cx.emit(SidebarEvent::Selected);
    }

    /// Select one of the first nine rows currently visible in the session list.
    pub fn select_shortcut(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project.as_ref() else {
            return;
        };
        // The rows the list shows, which the status filter narrows too: a
        // chord must never pick a row the reader cannot see.
        let rows: Vec<SessionRow> = self
            .rows
            .iter()
            .filter(|row| self.status.admits(row.state))
            .cloned()
            .collect();
        let Some(workspace) = session_shortcuts(&rows, project, &self.search_query)
            .get(index)
            .cloned()
        else {
            return;
        };
        self.select_workspace(&workspace, cx);
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
        self.place = Place::Workspace;
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
            .when(self.local_paths, |this| {
                this.child(
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
            })
    }

    /// The heading a project's workspaces hang under, and the way to aim a new
    /// chat at it.
    fn project_header(
        &self,
        project: ProjectName,
        label: SharedString,
        index: usize,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        // The project rail and the session list are separate navigation
        // levels, so both stay selected while a conversation is open.
        let selected =
            self.place == Place::Workspace && self.selected_project.as_ref() == Some(&project);
        if let Some((_, field)) = self
            .labelling
            .as_ref()
            .filter(|(labelling, _)| labelling == &project)
        {
            return div()
                .w_full()
                .px_1()
                .py_0p5()
                .child(Input::new(field))
                .into_any_element();
        }
        let menu_open = self.project_menu_for.as_ref() == Some(&project);
        let menu = menu_open.then(|| {
            let action = |id: String, label: String| {
                div()
                    .id(SharedString::from(id))
                    .px_2()
                    .py_0p5()
                    .rounded(px(tokens.radius.control()))
                    .text_xs()
                    .text_color(tokens.colors().text_secondary)
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_active()))
                    .child(label)
            };
            let (rename, up, down) = (project.clone(), project.clone(), project.clone());
            let current = label.clone();
            h_flex()
                .ml_5()
                .pb_1()
                .gap_1()
                .child(
                    action(
                        format!("project-rename:{}", project.0),
                        rust_i18n::t!("sidebar.action.rename").to_string(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_labelling(rename.clone(), current.clone(), window, cx)
                    })),
                )
                .when(index > 0, |this| {
                    this.child(
                        action(
                            format!("project-up:{}", project.0),
                            rust_i18n::t!("sidebar.action.move_up").to_string(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.project_menu_for = None;
                            cx.emit(SidebarEvent::MoveProject {
                                project: up.clone(),
                                index: index as u32 - 1,
                            });
                        })),
                    )
                })
                .when(index + 1 < count, |this| {
                    this.child(
                        action(
                            format!("project-down:{}", project.0),
                            rust_i18n::t!("sidebar.action.move_down").to_string(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.project_menu_for = None;
                            cx.emit(SidebarEvent::MoveProject {
                                project: down.clone(),
                                index: index as u32 + 1,
                            });
                        })),
                    )
                })
        });
        let toggle = project.clone();
        v_flex()
            .w_full()
            .child(self.project_row(project, label, selected, menu_open, toggle, cx))
            .children(menu)
            .into_any_element()
    }

    /// The rail's row for one project, with the ⋯ that opens its menu.
    fn project_row(
        &self,
        project: ProjectName,
        label: SharedString,
        selected: bool,
        menu_open: bool,
        toggle: ProjectName,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
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
            .child(
                div()
                    .id(SharedString::from(format!("project-menu:{}", toggle.0)))
                    .px_1()
                    .rounded(px(tokens.radius.control()))
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .cursor_pointer()
                    .when(menu_open, |this| this.bg(tokens.colors().row_active()))
                    .hover(|this| this.bg(tokens.colors().row_active()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.project_menu_for = if this.project_menu_for.as_ref() == Some(&toggle) {
                            None
                        } else {
                            Some(toggle.clone())
                        };
                        cx.notify();
                    }))
                    .child("⋯"),
            )
    }

    /// Start renaming a project in the rail, with its name in a focused field.
    fn start_labelling(
        &mut self,
        project: ProjectName,
        current: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(current.to_string(), window, cx);
            state
        });
        field.read(cx).focus_handle(cx).focus(window, cx);
        cx.subscribe(&field, |this, field, event: &InputEvent, cx| match event {
            InputEvent::PressEnter { .. } => {
                let label = field.read(cx).value().trim().to_string();
                if let Some((project, _)) = this.labelling.take() {
                    cx.emit(SidebarEvent::LabelProject { project, label });
                }
                cx.notify();
            }
            InputEvent::Blur => {
                this.labelling = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        self.project_menu_for = None;
        self.labelling = Some((project, field));
        cx.notify();
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
        let menu_open = self.menu_for.as_ref() == Some(&row.workspace);
        let actions = menu_open.then(|| self.row_actions(row, cx).into_any_element());
        let tokens = Tokens::global(cx);
        let selected = self.selected.as_ref() == Some(&row.workspace);
        let branch = row.branch_worth_showing();
        let workspace = row.workspace.clone();
        let rename_field = self
            .renaming
            .as_ref()
            .filter(|(renaming, _, _)| renaming == &row.workspace)
            .map(|(_, _, field)| field.clone());

        let menu_workspace = row.workspace.clone();

        v_flex()
            .w_full()
            .child(
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
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_workspace(&workspace, cx)),
                    )
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
                            .child(match rename_field {
                                Some(field) => div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&field))
                                    .into_any_element(),
                                None => div()
                                    .flex_1()
                                    .text_sm()
                                    .when(selected, |this| this.font_medium())
                                    .text_color(if selected {
                                        tokens.colors().text_primary
                                    } else {
                                        tokens.colors().text_secondary
                                    })
                                    .truncate()
                                    .child(row.title.clone())
                                    .into_any_element(),
                            })
                            .when(row.pinned, |this| {
                                this.child(
                                    Icon::new(IconName::StarFill)
                                        .size_3()
                                        .text_color(tokens.colors().text_muted),
                                )
                            })
                            .when(row.queued > 0, |this| {
                                // How much is waiting, in a word and a number,
                                // so a held queue is findable from the list.
                                this.child(
                                    div()
                                        .px_1()
                                        .rounded(px(tokens.radius.control()))
                                        .bg(tokens.colors().row_active())
                                        .text_xs()
                                        .text_color(tokens.colors().text_secondary)
                                        .child(
                                            rust_i18n::t!("sidebar.queued", count = row.queued)
                                                .to_string(),
                                        ),
                                )
                            })
                            .child(self.status(row, cx))
                            .child(
                                div()
                                    .id(("session-menu", index))
                                    .px_0p5()
                                    .rounded(px(tokens.radius.control()))
                                    .cursor_pointer()
                                    .when(menu_open, |this| this.bg(tokens.colors().row_active()))
                                    .hover(|this| this.bg(tokens.colors().row_active()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.menu_for = (this.menu_for.as_ref()
                                            != Some(&menu_workspace))
                                        .then(|| menu_workspace.clone());
                                        cx.notify();
                                    }))
                                    .child(
                                        Icon::new(IconName::Ellipsis)
                                            .size_3p5()
                                            .text_color(tokens.colors().text_muted),
                                    ),
                            ),
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
                    }),
            )
            .children(actions)
    }

    /// Start renaming a conversation, with its title in a focused field.
    fn start_rename(
        &mut self,
        workspace: WorkspaceId,
        session: SessionId,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(title.to_string(), window, cx);
            state
        });
        field.read(cx).focus_handle(cx).focus(window, cx);
        cx.subscribe(&field, |this, field, event: &InputEvent, cx| match event {
            InputEvent::PressEnter { .. } => {
                let title = field.read(cx).value().trim().to_string();
                if let Some((_, session, _)) = this.renaming.take() {
                    cx.emit(SidebarEvent::Rename { session, title });
                }
                cx.notify();
            }
            InputEvent::Blur => {
                this.renaming = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        self.menu_for = None;
        self.renaming = Some((workspace, session, field));
        cx.notify();
    }

    /// The row's actions, under it while its menu is open.
    fn row_actions(&self, row: &SessionRow, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let action = |id: String, label: String| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py_0p5()
                .rounded(px(tokens.radius.control()))
                .text_xs()
                .text_color(tokens.colors().text_secondary)
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .child(label)
        };
        let key = row.workspace.0.clone();
        let (pin_ws, archive_ws) = (row.workspace.clone(), row.workspace.clone());
        let pinned = row.pinned;
        let rename = row.session.clone().map(|session| {
            let workspace = row.workspace.clone();
            let title = row.title.clone();
            action(
                format!("rename:{key}"),
                rust_i18n::t!("sidebar.action.rename").to_string(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.start_rename(
                    workspace.clone(),
                    session.clone(),
                    title.clone(),
                    window,
                    cx,
                );
            }))
        });
        h_flex()
            .ml_3()
            .px_2()
            .pb_1()
            .gap_1()
            .children(rename)
            .child(
                action(
                    format!("pin:{key}"),
                    if pinned {
                        rust_i18n::t!("sidebar.action.unpin").to_string()
                    } else {
                        rust_i18n::t!("sidebar.action.pin").to_string()
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu_for = None;
                    cx.emit(SidebarEvent::Pin {
                        workspace: pin_ws.clone(),
                        pinned: !pinned,
                    });
                    cx.notify();
                })),
            )
            .child(
                action(
                    format!("archive:{key}"),
                    rust_i18n::t!("sidebar.action.archive").to_string(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu_for = None;
                    cx.emit(SidebarEvent::Archive {
                        workspace: archive_ws.clone(),
                        archived: true,
                    });
                    cx.notify();
                })),
            )
    }

    /// One item of the session list (`ginka_ui::session_list::Entry`).
    fn session_entry(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        use ginka_ui::session_list::Entry;
        let Some(entry) = self.entries.get(index).cloned() else {
            return div().into_any_element();
        };
        let item = div().w_full().pb_0p5();
        match entry {
            Entry::Project(name) => {
                let position = self
                    .projects
                    .iter()
                    .position(|project| project.name == name)
                    .unwrap_or_default();
                let label = self
                    .projects
                    .get(position)
                    .map(|project| project.label.clone())
                    .unwrap_or_else(|| name.0.clone().into());
                let count = self.projects.len();
                item.child(self.project_header(name, label, position, count, cx))
                    .into_any_element()
            }
            Entry::Row(row) => match self.rows.get(row).cloned() {
                Some(session) => item
                    .child(self.session_row(row, &session, cx))
                    .into_any_element(),
                None => item.into_any_element(),
            },
            Entry::ArchivedHeading => item
                .pt_2()
                .child(self.archived_header(cx))
                .into_any_element(),
            Entry::Archived(row) => match self.rows.get(row).cloned() {
                Some(session) => item
                    .child(self.archived_row(row, &session, cx))
                    .into_any_element(),
                None => item.into_any_element(),
            },
            Entry::ArchivedFoot => {
                let tokens = Tokens::global(cx);
                item.child(
                    div()
                        .px_2p5()
                        .py_1p5()
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("sidebar.show_more", count = 25).to_string()),
                )
                .into_any_element()
            }
        }
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
            .child({
                let workspace = row.workspace.clone();
                div()
                    .id(("restore", index))
                    .px_2()
                    .py_0p5()
                    .rounded(px(tokens.radius.control()))
                    .text_xs()
                    .text_color(tokens.colors().text_secondary)
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_active()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SidebarEvent::Archive {
                            workspace: workspace.clone(),
                            archived: false,
                        });
                    }))
                    .child(rust_i18n::t!("sidebar.action.restore").to_string())
            })
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
            .filter(|(_, row)| self.status.admits(row.state))
            .filter(|(_, row)| ginka_ui::workspace::session_matches(row, &self.search_query))
            .map(|(index, row)| (index, row.clone()))
            .collect();
        // Stable: equal ranks keep their insertion order.
        // Pinned first, then the attention sort; stable, so equal ranks keep
        // their insertion order.
        active.sort_by_key(|(_, row)| row.list_rank());
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
        let headings = !(selected_project_name.is_some() && self.place == Place::Workspace);
        let archived_indices: Vec<usize> = archived.iter().map(|(index, _)| *index).collect();
        let entries = ginka_ui::session_list::entries(
            &groups,
            headings,
            &archived_indices,
            self.archived_open,
        );
        if let Some(from) = ginka_ui::session_list::first_difference(&self.entries, &entries) {
            let old = self.entries.len();
            self.session_list.splice(from..old, entries.len() - from);
        }
        self.entries = entries;

        let project_rows = self.projects.clone();
        let selected_project = selected_project_name.is_some() && self.place == Place::Workspace;
        let settings_hint = if cfg!(target_os = "macos") {
            "⌘,"
        } else {
            "Ctrl ,"
        };
        let inbox = self.place_row(
            Place::Inbox,
            Icon::new(IconName::Inbox),
            rust_i18n::t!("nav.inbox").to_string(),
            None,
            cx,
        );
        let notes = self.place_row(
            Place::Notes,
            Icon::empty().path(ginka_ui::assets::icon::NOTEBOOK),
            rust_i18n::t!("nav.notes").to_string(),
            None,
            cx,
        );
        let settings = self.place_row(
            Place::Settings,
            Icon::new(IconName::Settings),
            rust_i18n::t!("nav.settings").to_string(),
            Some(settings_hint),
            cx,
        );

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
                    .child(v_flex().px_1p5().pb_1().gap_0p5().child(inbox).child(notes))
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
                            .children({
                                let count = project_rows.len();
                                project_rows
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, project)| {
                                        self.project_header(
                                            project.name,
                                            project.label,
                                            index,
                                            count,
                                            cx,
                                        )
                                        .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                    )
                    .child(div().px_1p5().py_1p5().child(settings)),
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
                                .child(
                                    div()
                                        .id("session-status-filter")
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded(px(tokens.radius.control()))
                                        .cursor_pointer()
                                        .when(
                                            self.status != ginka_ui::workspace::StatusFilter::All,
                                            |this| this.bg(tokens.colors().row_active()),
                                        )
                                        .hover(|this| this.bg(tokens.colors().row_active()))
                                        .text_xs()
                                        .text_color(tokens.colors().text_secondary)
                                        .child(self.status.label())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.status = this.status.next();
                                            cx.notify();
                                        })),
                                )
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
                                .child(
                                    list(
                                        self.session_list.clone(),
                                        cx.processor(|this, index: usize, _window, cx| {
                                            this.session_entry(index, cx)
                                        }),
                                    )
                                    .flex_1()
                                    .size_full(),
                                ),
                        )
                        .child(self.footer(cx)),
                )
            })
    }
}
