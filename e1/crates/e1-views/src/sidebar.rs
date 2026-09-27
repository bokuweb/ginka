//! The navigation column: `docs/ui.md` §3.2.
//!
//! Four fixed sections, then the repositories. It reads the store and emits
//! what the reader picked; the shell decides what the centre column does
//! with that, so the sidebar does not know what a list is and the shell does
//! not know how a row is drawn.

use crate::avatar::avatar;
use crate::store::{Store, StoreEvent};
use e1_github::Repo;
use e1_ui::assets::icon;
use e1_ui::theme::ThemeAppearance;
use e1_ui::{Focus, Section, Tokens, group_by_owner};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};
use std::collections::HashSet;

/// Emitted when the reader picks a row.
pub enum SidebarEvent {
    /// Show this in the centre column.
    Focus(Focus),
    /// Forget the token and go back to the sign-in screen.
    SignOut,
    /// Flip the window between light and dark.
    ToggleAppearance,
    /// An owner's repositories were folded away, or shown again.
    OwnerToggled {
        /// Which owner.
        owner: String,
        /// Folded, now.
        collapsed: bool,
    },
}

impl EventEmitter<SidebarEvent> for Sidebar {}

/// The navigation column.
pub struct Sidebar {
    store: Entity<Store>,
    selected: Option<Focus>,
    /// The owners whose repositories are folded away.
    collapsed: HashSet<String>,
}

impl Sidebar {
    /// A sidebar over a store. It redraws whenever the store changes, because
    /// the inbox count and the repository list are the store's.
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&store, |this, _, _: &StoreEvent, cx| {
            let url = this
                .store
                .read(cx)
                .viewer()
                .value()
                .map(|viewer| viewer.avatar_url.clone());
            if let Some(url) = url {
                this.store
                    .update(cx, |store, cx| store.ensure_avatar(&url, cx));
            }
            cx.notify();
        })
        .detach();
        Self {
            store,
            selected: None,
            collapsed: HashSet::new(),
        }
    }

    /// Fold these owners' repositories away, as the settings remember.
    pub fn set_collapsed(
        &mut self,
        owners: impl IntoIterator<Item = String>,
        cx: &mut Context<Self>,
    ) {
        self.collapsed = owners.into_iter().collect();
        cx.notify();
    }

    fn toggle_owner(&mut self, owner: String, cx: &mut Context<Self>) {
        let collapsed = if self.collapsed.remove(&owner) {
            false
        } else {
            self.collapsed.insert(owner.clone());
            true
        };
        cx.emit(SidebarEvent::OwnerToggled { owner, collapsed });
        cx.notify();
    }

    /// The heading an owner's repositories hang under. Picking it folds them.
    fn owner_row(
        &self,
        owner: &str,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        let collapsed = self.collapsed.contains(owner);
        let name = owner.to_string();
        h_flex()
            .id(SharedString::from(format!("owner:{owner}")))
            .w_full()
            .px_2p5()
            .py_1()
            .gap_1p5()
            .items_center()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_owner(name.clone(), cx)))
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size_3()
                .text_color(tokens.colors().text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(tokens.colors().text_muted)
                    .truncate()
                    .child(owner.to_string()),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(count.to_string()),
            )
    }

    /// Pick something and say so.
    pub fn select(&mut self, focus: Focus, cx: &mut Context<Self>) {
        self.selected = Some(focus.clone());
        cx.emit(SidebarEvent::Focus(focus));
        cx.notify();
    }

    /// Move the highlight without announcing it, for when the shell changed
    /// the list itself (a kind toggle) and announcing it back would have the
    /// shell answer its own message.
    pub fn adopt(&mut self, focus: Focus, cx: &mut Context<Self>) {
        self.selected = Some(focus);
        cx.notify();
    }

    /// Highlight nothing: a search belongs to no row.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        cx.notify();
    }

    /// What is highlighted.
    pub fn selected(&self) -> Option<&Focus> {
        self.selected.as_ref()
    }

    /// A small muted label over a run of rows.
    fn section_label(&self, label: String, cx: &App) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        bgpui_kit::section_label(label, &tokens.as_shared()).text_size(px(11.5))
    }

    /// One of the four fixed rows.
    fn section_row(&self, section: Section, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        let focus = Focus::Section(section);
        let selected = self.selected.as_ref() == Some(&focus);
        let count: Option<usize> = match section {
            Section::Inbox => self
                .store
                .read(cx)
                .inbox()
                .value()
                .map(|inbox| inbox.iter().filter(|n| n.unread).count())
                .filter(|count| *count > 0),
            Section::Projects => self
                .store
                .read(cx)
                .all_projects()
                .value()
                .map(Vec::len)
                .filter(|count| *count > 0),
            other => self
                .store
                .read(cx)
                .list(&Focus::Section(other))
                .and_then(|list| list.value())
                .map(|items| items.len())
                .filter(|count| *count > 0),
        };
        h_flex()
            .id(SharedString::from(format!("section:{section:?}")))
            .w_full()
            .px_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .when(selected, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.select(focus.clone(), cx)))
            .child(
                Icon::empty()
                    .path(section.icon())
                    .size_4()
                    .text_color(if selected {
                        tokens.colors().text_primary
                    } else {
                        tokens.colors().text_secondary
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .when(selected, |this| this.font_medium())
                    .text_color(if selected {
                        tokens.colors().text_primary
                    } else {
                        tokens.colors().text_secondary
                    })
                    .child(rust_i18n::t!(section.label_key()).to_string()),
            )
            .children(count.map(|count| {
                div()
                    .text_size(px(11.5))
                    .text_color(if section == Section::Inbox {
                        tokens.colors().accent
                    } else {
                        tokens.colors().text_muted
                    })
                    .child(count.to_string())
            }))
    }

    /// A repository.
    fn repo_row(
        &self,
        index: usize,
        repo: &Repo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        let selected = self
            .selected
            .as_ref()
            .and_then(Focus::repo_id)
            .is_some_and(|id| id == &repo.id);
        let id = repo.id.clone();
        h_flex()
            .id(("repo", index))
            .w_full()
            // Indented under the owner heading: the indent is what says
            // these belong to it. Padding rather than a margin, so the row
            // still ends where the column does and the lock is not pushed
            // against the edge.
            .pl_6()
            .pr_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .when(selected, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.select(Focus::repo(id.clone()), cx)))
            .child(
                h_flex()
                    .flex_1()
                    .overflow_hidden()
                    .text_size(px(13.))
                    .when(selected, |this| this.font_medium())
                    .text_color(if selected {
                        tokens.colors().text_primary
                    } else {
                        tokens.colors().text_secondary
                    })
                    .child(div().truncate().child(repo.id.name.clone())),
            )
            .when(repo.private, |this| {
                this.child(
                    Icon::empty()
                        .path(icon::LOCK)
                        .size_3()
                        .text_color(tokens.colors().text_muted),
                )
            })
    }

    /// The footer's appearance control: what the window is set to, and a
    /// click to move to the next.
    fn appearance_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        // What is drawn is the theme that is installed, not a copy of the
        // setting: there may be no setting yet, and the two cannot drift if
        // only one of them exists.
        let (path, tip) = match tokens.appearance {
            ThemeAppearance::Dark => (icon::MOON, rust_i18n::t!("sidebar.appearance.dark")),
            ThemeAppearance::Light => (icon::SUN, rust_i18n::t!("sidebar.appearance.light")),
        };
        let tip = tip.to_string();
        div()
            .id("appearance")
            .p_1()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(
                Icon::empty()
                    .path(path)
                    .size_3p5()
                    .text_color(tokens.colors().text_muted),
            )
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::ToggleAppearance)))
    }

    /// Whose window this is.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Built first: it binds a listener through `cx`, and the tokens
        // borrowed below are held across the rest of the strip.
        let appearance_button = self.appearance_button(cx).into_any_element();
        let tokens = Tokens::global(cx);
        let viewer = self.store.read(cx).viewer().value().cloned();
        let signed_in = viewer.is_some();
        let picture = viewer
            .as_ref()
            .and_then(|viewer| self.store.read(cx).avatar(&viewer.avatar_url));
        let (initial, login): (String, SharedString) = match viewer {
            Some(viewer) => (
                viewer.login.clone(),
                viewer.name.clone().unwrap_or(viewer.login).into(),
            ),
            None => (
                "?".into(),
                rust_i18n::t!("app.signed_out").to_string().into(),
            ),
        };
        let picture = avatar(picture, &initial, px(24.), cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2p5()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
            .child(picture)
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(tokens.colors().text_secondary)
                    .truncate()
                    .child(login),
            )
            .child(appearance_button)
            .when(signed_in, |this| {
                this.child(
                    div()
                        .id("sign-out")
                        .p_1()
                        .rounded(px(tokens.radius.row))
                        .cursor_pointer()
                        .hover(|this| this.bg(tokens.colors().row_hover()))
                        .tooltip(|window, cx| {
                            Tooltip::new(rust_i18n::t!("sidebar.sign_out").to_string())
                                .build(window, cx)
                        })
                        .child(
                            Icon::new(IconName::CircleX)
                                .size_3p5()
                                .text_color(tokens.colors().text_muted),
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::SignOut))),
                )
            })
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let repos: Vec<Repo> = self
            .store
            .read(cx)
            .repos()
            .value()
            .cloned()
            .unwrap_or_default();
        let repos_error = self.store.read(cx).repos().error().map(str::to_string);

        let sections: Vec<AnyElement> = Section::ALL
            .iter()
            .map(|section| self.section_row(*section, cx).into_any_element())
            .collect();
        let mut repo_rows: Vec<AnyElement> = Vec::new();
        let mut index = 0;
        for group in group_by_owner(&repos) {
            repo_rows.push(
                self.owner_row(&group.owner, group.repos.len(), cx)
                    .into_any_element(),
            );
            let folded = self.collapsed.contains(&group.owner);
            for repo in &group.repos {
                if !folded {
                    repo_rows.push(self.repo_row(index, repo, cx).into_any_element());
                }
                index += 1;
            }
        }

        v_flex()
            .size_full()
            .bg(tokens.colors().bg_sidebar)
            .border_r_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                v_flex()
                    .id("sidebar-scroll")
                    .flex_1()
                    .px_2()
                    .overflow_y_scroll()
                    .children(sections)
                    .child(
                        self.section_label(rust_i18n::t!("sidebar.repositories").to_string(), cx),
                    )
                    .children(repo_rows)
                    .when(repos.is_empty(), |this| {
                        this.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(px(11.5))
                                .text_color(match &repos_error {
                                    Some(_) => tokens.colors().status_error,
                                    None => tokens.colors().text_muted,
                                })
                                .child(repos_error.unwrap_or_else(|| {
                                    rust_i18n::t!("sidebar.repositories.empty").to_string()
                                })),
                        )
                    }),
            )
            .child(self.footer(cx))
    }
}
