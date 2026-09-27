//! The centre column: `docs/ui.md` §3.3.
//!
//! A virtualized list of rows, each precomputed from the store's answer when
//! it lands (`AGENTS.md` rule 7): the list only draws the rows on screen, and
//! a row is drawn from strings that were formatted once, not on every scroll.

use crate::avatar::avatar;
use crate::store::{ItemKey, Store, StoreEvent};
use chrono::Utc;
use e1_github::{CheckState, Project};
use e1_ui::assets::icon;
use e1_ui::rows::{Glyph, ItemRow};
use e1_ui::{Focus, Section, Tokens};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};

/// How tall a row is. Two lines and their air; the same for every row, which
/// is what lets the list be uniform.
const ROW_HEIGHT: Pixels = px(56.);

/// Emitted when the reader does something with a row.
pub enum ItemEvent {
    /// Open an item in the detail panel.
    Open {
        /// Which item.
        key: ItemKey,
        /// Whether the row said it was a pull, which saves the detail a
        /// round trip.
        is_pull: bool,
    },
    /// Open a Project in the native detail panel.
    OpenProject(Project),
    /// Open something on the web, because it has no detail here.
    OpenUrl(String),
}

impl EventEmitter<ItemEvent> for ItemList {}

/// The centre column.
pub struct ItemList {
    store: Entity<Store>,
    focus: Option<Focus>,
    rows: Vec<ItemRow>,
    projects: Vec<Project>,
    selected: Option<ItemKey>,
    selected_project: Option<String>,
}

impl ItemList {
    /// A list over a store, showing nothing until told what to.
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&store, |this, _, _: &StoreEvent, cx| this.rebuild(cx))
            .detach();
        Self {
            store,
            focus: None,
            rows: Vec::new(),
            projects: Vec::new(),
            selected: None,
            selected_project: None,
        }
    }

    /// What is being listed.
    pub fn focus(&self) -> Option<&Focus> {
        self.focus.as_ref()
    }

    /// Whether the list is being fetched.
    pub fn is_loading(&self, cx: &App) -> bool {
        let store = self.store.read(cx);
        match &self.focus {
            None => false,
            Some(Focus::Section(Section::Inbox)) => store.inbox().is_loading(),
            Some(Focus::Section(Section::Projects)) => store.all_projects().is_loading(),
            Some(focus) => store.list(focus).is_some_and(|list| list.is_loading()),
        }
    }

    /// List something, fetching it if it never has been.
    pub fn set_focus(&mut self, focus: Focus, cx: &mut Context<Self>) {
        if self.focus.as_ref() != Some(&focus) {
            self.selected = None;
            self.selected_project = None;
        }
        self.focus = Some(focus.clone());
        self.store
            .update(cx, |store, cx| store.ensure_list(focus, cx));
        self.rebuild(cx);
    }

    /// Fetch the list again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(focus) = self.focus.clone() {
            self.store
                .update(cx, |store, cx| store.load_list(focus, cx));
        }
    }

    /// Rebuild the rows from the store's current answer.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let now = Utc::now();
        let muted = Tokens::global(cx).colors().text_muted;
        let store = self.store.read(cx);
        self.projects = if self.focus == Some(Focus::Section(Section::Projects)) {
            store.all_projects().value().cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        self.rows = match &self.focus {
            None => Vec::new(),
            Some(Focus::Section(Section::Inbox)) => store
                .inbox()
                .value()
                .map(|inbox| {
                    inbox
                        .iter()
                        .map(|notification| {
                            let check = notification.number.and_then(|number| {
                                store.status(&(notification.repo.clone(), number))
                            });
                            ItemRow::from_notification(notification, now).with_check(check)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Some(Focus::Section(Section::Projects)) => store
                .all_projects()
                .value()
                .map(|projects| projects.iter().map(ItemRow::from_project).collect())
                .unwrap_or_default(),
            Some(focus) => store
                .list(focus)
                .and_then(|list| list.value())
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            let check = store.status(&(item.repo.clone(), item.number));
                            ItemRow::from_item(item, now, muted).with_check(check)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let urls: Vec<String> = self
            .rows
            .iter()
            .filter_map(|row| row.avatar_url.clone())
            .collect();
        self.store.update(cx, |store, cx| {
            for url in urls {
                store.ensure_avatar(&url, cx);
            }
        });
        cx.notify();
    }

    /// The reader picked a row.
    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(project) = self.projects.get(index).cloned() {
            self.selected_project = Some(project.id.clone());
            self.selected = None;
            cx.emit(ItemEvent::OpenProject(project));
            cx.notify();
            return;
        }
        let Some(row) = self.rows.get(index) else {
            return;
        };
        match (&row.key, &row.html_url) {
            (Some(key), _) => {
                self.selected = Some(key.clone());
                let is_pull = matches!(
                    row.glyph,
                    Glyph::PullOpen | Glyph::PullDraft | Glyph::PullMerged | Glyph::PullClosed
                );
                cx.emit(ItemEvent::Open {
                    key: key.clone(),
                    is_pull,
                });
                cx.notify();
            }
            (None, Some(url)) => cx.emit(ItemEvent::OpenUrl(url.clone())),
            (None, None) => {}
        }
    }

    /// One row. Fixed height, two lines.
    fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx);
        let Some(row) = self.rows.get(index) else {
            return div().h(ROW_HEIGHT).into_any_element();
        };
        let selected = (row.key.is_some() && self.selected == row.key)
            || self
                .projects
                .get(index)
                .is_some_and(|project| Some(&project.id) == self.selected_project.as_ref());
        let glyph_color = row.glyph.role().color(tokens.colors());
        let picture = row
            .avatar_url
            .as_deref()
            .and_then(|url| self.store.read(cx).avatar(url));
        let author = row.avatar_url.as_ref().map(|_| {
            avatar(
                picture,
                row.meta.split(" · ").next().unwrap_or_default(),
                px(18.),
                cx,
            )
        });
        // Lists that span repositories say which one each row is from.
        let spans_repos = matches!(self.focus, Some(Focus::Section(_) | Focus::Search { .. }));

        div()
            .h(ROW_HEIGHT)
            .w_full()
            .px_2()
            .py_0p5()
            .child(
                h_flex()
                    .id(("row", index))
                    .size_full()
                    .px_2p5()
                    .gap_2p5()
                    .items_center()
                    .rounded(px(tokens.radius.row))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(tokens.colors().row_active()))
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(index, cx)))
                    .child(
                        // The unread dot sits where the glyph's colour already
                        // is, so an unread row is louder without a second mark
                        // in the text.
                        div()
                            .relative()
                            .flex_shrink_0()
                            .child(
                                Icon::empty()
                                    .path(row.glyph.icon())
                                    .size_4()
                                    .text_color(glyph_color),
                            )
                            .when(row.unread, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top(px(-2.))
                                        .right(px(-3.))
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(tokens.colors().accent),
                                )
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .overflow_hidden()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(13.))
                                            .when(row.unread || selected, |this| this.font_medium())
                                            .text_color(if row.unread || selected {
                                                tokens.colors().text_primary
                                            } else {
                                                tokens.colors().text_secondary
                                            })
                                            .truncate()
                                            .child(row.title.clone()),
                                    )
                                    .children(row.check.map(|check| {
                                        // How the checks stand, the way GitHub's
                                        // list shows it: a mark beside the number.
                                        let colors = tokens.colors();
                                        match check {
                                            CheckState::Success => Icon::new(IconName::Check)
                                                .size_3()
                                                .text_color(colors.status_done)
                                                .into_any_element(),
                                            CheckState::Failure => Icon::new(IconName::Close)
                                                .size_3()
                                                .text_color(colors.status_error)
                                                .into_any_element(),
                                            CheckState::Pending => div()
                                                .size_2()
                                                .rounded_full()
                                                .bg(colors.status_attention)
                                                .into_any_element(),
                                            CheckState::Neutral => div().into_any_element(),
                                        }
                                    }))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(tokens.colors().text_muted)
                                            .child(row.number.clone()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .items_center()
                                    .overflow_hidden()
                                    .when(spans_repos, |this| {
                                        this.child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(tokens.colors().text_secondary)
                                                .truncate()
                                                .child(row.repo.clone()),
                                        )
                                    })
                                    .children(author)
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(tokens.colors().text_muted)
                                            .truncate()
                                            .child(row.meta.clone()),
                                    )
                                    .children(row.comments.map(|count| {
                                        h_flex()
                                            .gap_1()
                                            .items_center()
                                            .text_size(px(11.5))
                                            .text_color(tokens.colors().text_muted)
                                            .child(
                                                Icon::empty()
                                                    .path(icon::MESSAGE)
                                                    .size_3()
                                                    .text_color(tokens.colors().text_muted),
                                            )
                                            .child(count.to_string())
                                    }))
                                    .children(row.labels.iter().map(|label| {
                                        let (fill, text) = label.paint(tokens.appearance);
                                        div()
                                            .px_1p5()
                                            .rounded(px(tokens.radius.row))
                                            .bg(fill)
                                            .text_size(px(11.5))
                                            .text_color(text)
                                            .child(label.name.clone())
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// A single muted line in the middle of the column.
    fn notice(&self, text: String, error: bool, cx: &App) -> AnyElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px_8()
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(if error {
                        tokens.colors().status_error
                    } else {
                        tokens.colors().text_muted
                    })
                    .text_center()
                    .child(text),
            )
            .into_any_element()
    }
}

impl Render for ItemList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = {
            let store = self.store.read(cx);
            match &self.focus {
                None => None,
                Some(Focus::Section(Section::Inbox)) => store.inbox().error().map(str::to_string),
                Some(Focus::Section(Section::Projects)) => {
                    store.all_projects().error().map(str::to_string)
                }
                Some(focus) => store
                    .list(focus)
                    .and_then(|list| list.error())
                    .map(str::to_string),
            }
        };
        let loading = self.is_loading(cx);

        let body: AnyElement = if self.focus.is_none() {
            self.notice(rust_i18n::t!("list.pick").to_string(), false, cx)
        } else if self.rows.is_empty() {
            match error {
                Some(error) => self.notice(error, true, cx),
                None if loading => crate::skeleton::list_rows(8, cx),
                None => self.notice(
                    rust_i18n::t!(if self.focus == Some(Focus::Section(Section::Projects)) {
                        "list.projects.empty"
                    } else {
                        "list.empty"
                    })
                    .to_string(),
                    false,
                    cx,
                ),
            }
        } else {
            let this = cx.entity();
            uniform_list("items", self.rows.len(), move |range, _window, cx| {
                this.update(cx, |this, cx| {
                    range.map(|index| this.row(index, cx)).collect()
                })
            })
            .flex_1()
            .size_full()
            .py_1()
            .into_any_element()
        };
        let body = match (&self.focus, self.rows.is_empty()) {
            (Some(focus), false) => crate::fade::fade_in(
                SharedString::from(format!("list:{}", focus.title())),
                body,
                cx,
            ),
            _ => body,
        };

        v_flex().size_full().child(body)
    }
}
