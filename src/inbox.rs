//! The Inbox: `docs/ui.md` §3.5.
//!
//! Pull requests and issues, from the GitHub client (`e1`), mounted as pieces
//! rather than as its whole window: its list and its detail, laid out the way
//! MonoCode lays its inbox out — a list column beside the navigation with the
//! connections across its top, the item in the centre, and the agent the
//! reader *Ask*s about it at the far right. e1's own navigation, window
//! controls and palette stay behind; Ginka's are already on screen.
//!
//! Built only with `--features github`. Without it the Inbox says so, and how
//! to build it in, rather than disappearing from the navigation: a reader
//! looking for their pull requests should learn where they went.

use ginka_ui::Tokens;
use ginka_ui::layout::HEADER_HEIGHT;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, h_flex, v_flex};

#[cfg(feature = "github")]
mod mounted {
    use super::*;
    use e1_github::{GitHub, HttpCache, Rest};
    use e1_ui::{Focus, Section};
    use e1_views::Store;
    use e1_views::agent::{AgentPane, AgentPaneEvent};
    use e1_views::detail::{Detail, DetailEvent};
    use e1_views::list::{ItemEvent, ItemList};
    use e1_views::signin::{SignIn, SignInEvent};
    use e1_views::store::StoreEvent;
    use std::sync::Arc;

    /// Tells the host whether anything is unread, for the dot in the nav.
    pub struct InboxUnread(pub bool);

    impl EventEmitter<InboxUnread> for Inbox {}

    pub struct Inbox {
        store: Entity<Store>,
        list: Entity<ItemList>,
        detail: Entity<Detail>,
        agent: Entity<AgentPane>,
        sign_in: Entity<SignIn>,
        signed_in: bool,
        section: Section,
        /// Whether the detail has an item: until one is chosen the centre
        /// says to choose one.
        showing: bool,
        /// Whether the *Ask* pane is open.
        asking: bool,
        paths: e1_ui::Paths,
        _subscriptions: Vec<Subscription>,
    }

    impl Inbox {
        pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            // e1's settings and caches are its own, under `~/.e1`: the daemon
            // does not own them yet, and putting them under Ginka's directory
            // would claim it does.
            let paths = e1_ui::Paths::from_env().expect("no home directory");
            if let Err(error) = paths.ensure() {
                tracing::warn!(%error, "the GitHub client's directory could not be created");
            }
            let found = e1_github::auth::discover();
            let signed_in = found.is_some();
            let github: Arc<dyn GitHub> = match found {
                Some((token, _)) => {
                    Arc::new(Rest::new(token).with_cache(HttpCache::new(paths.http_cache())))
                }
                None => Arc::new(e1_github::Scripted::empty()),
            };
            let avatars = paths.cache().join("avatars");
            let snapshot = paths.snapshot();
            let store = cx.new(|_| {
                let store = Store::new(github).with_avatars(avatars);
                if signed_in {
                    store.with_snapshot(snapshot)
                } else {
                    store.remembering(snapshot)
                }
            });
            let list = cx.new(|cx| ItemList::new(store.clone(), cx));
            let detail = cx.new(|cx| Detail::new(store.clone(), window, cx));
            let agent = cx.new(|cx| AgentPane::new(store.clone(), window, cx));
            let sign_in = cx.new(|_| SignIn::new());

            let subscriptions = vec![
                cx.subscribe(&list, |this, _, event, cx| match event {
                    ItemEvent::Open { key, is_pull } => {
                        let (key, is_pull) = (key.clone(), *is_pull);
                        this.detail
                            .update(cx, |detail, cx| detail.show(key, Some(is_pull), cx));
                        this.showing = true;
                        cx.notify();
                    }
                    ItemEvent::OpenProject(project) => {
                        this.detail
                            .update(cx, |detail, cx| detail.show_project(project.clone(), cx));
                        this.showing = true;
                        cx.notify();
                    }
                    ItemEvent::OpenUrl(url) => cx.open_url(url),
                }),
                cx.subscribe(&detail, |this, _, event, cx| match event {
                    DetailEvent::Ask(ask) => {
                        this.agent
                            .update(cx, |agent, cx| agent.open(ask.clone(), cx));
                        this.asking = true;
                        cx.notify();
                    }
                }),
                cx.subscribe(&agent, |this, _, event, cx| {
                    if let AgentPaneEvent::Close = event {
                        this.asking = false;
                        cx.notify();
                    }
                }),
                cx.subscribe(&sign_in, |this, _, event, cx| match event {
                    SignInEvent::SignedIn(token) => {
                        let cache = HttpCache::new(this.paths.http_cache());
                        let github: Arc<dyn GitHub> =
                            Arc::new(Rest::new(token.clone()).with_cache(cache));
                        this.signed_in = true;
                        this.store.update(cx, |store, cx| {
                            store.set_source(github, cx);
                            store.refresh_all(cx);
                        });
                        this.focus(Section::Inbox, cx);
                    }
                }),
                cx.subscribe(&store, |this, store, _: &StoreEvent, cx| {
                    let unread = store
                        .read(cx)
                        .inbox()
                        .value()
                        .is_some_and(|items| items.iter().any(|item| item.unread));
                    cx.emit(InboxUnread(unread && this.signed_in));
                    cx.notify();
                }),
            ];

            let mut this = Self {
                store,
                list,
                detail,
                agent,
                sign_in,
                signed_in,
                section: Section::Inbox,
                showing: false,
                asking: false,
                paths,
                _subscriptions: subscriptions,
            };
            this.store.update(cx, |store, cx| store.load_agents(cx));
            if signed_in {
                this.store.update(cx, |store, cx| store.refresh_all(cx));
                this.focus(Section::Inbox, cx);
            }
            this
        }

        fn focus(&mut self, section: Section, cx: &mut Context<Self>) {
            self.section = section;
            self.list
                .update(cx, |list, cx| list.set_focus(Focus::Section(section), cx));
            cx.notify();
        }

        /// Read everything again.
        pub fn refresh(&mut self, cx: &mut Context<Self>) {
            self.list.update(cx, |list, cx| list.refresh(cx));
        }

        fn list_column(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
            let tokens = Tokens::global(cx).clone();
            let sections = [
                (Section::Inbox, rust_i18n::t!("inbox.section.inbox")),
                (Section::MyPulls, rust_i18n::t!("inbox.section.mine")),
                (Section::Reviews, rust_i18n::t!("inbox.section.reviews")),
                (Section::Assigned, rust_i18n::t!("inbox.section.assigned")),
            ];
            v_flex()
                .size_full()
                .bg(tokens.colors().bg_sidebar)
                .border_r_1()
                .border_color(tokens.colors().border_subtle)
                .child(super::header(cx))
                .child(super::connections(true, cx))
                .child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_1()
                        .flex_wrap()
                        .border_b_1()
                        .border_color(tokens.colors().border_subtle)
                        .children(sections.into_iter().map(|(section, label)| {
                            let chosen = self.section == section;
                            div()
                                .id(SharedString::from(format!("inbox-section:{section:?}")))
                                .px_2()
                                .py_0p5()
                                .rounded(px(5.))
                                .text_size(px(11.5))
                                .cursor_pointer()
                                .when(chosen, |this| this.bg(tokens.colors().row_active()))
                                .hover(|this| this.bg(tokens.colors().row_hover()))
                                .text_color(if chosen {
                                    tokens.colors().text_primary
                                } else {
                                    tokens.colors().text_muted
                                })
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.focus(section, cx)),
                                )
                                .child(label.to_string())
                        }))
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("inbox-refresh")
                                .p_1()
                                .rounded(px(tokens.radius.control()))
                                .cursor_pointer()
                                .hover(|this| this.bg(tokens.colors().row_hover()))
                                .tooltip(|window, cx| {
                                    Tooltip::new(rust_i18n::t!("explorer.refresh").to_string())
                                        .build(window, cx)
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                                .child(
                                    Icon::new(IconName::RotateCw)
                                        .size_3p5()
                                        .text_color(tokens.colors().text_muted),
                                ),
                        ),
                )
                .child(div().flex_1().min_h_0().child(self.list.clone()))
        }
    }

    impl Render for Inbox {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tokens = Tokens::global(cx).clone();
            if !self.signed_in {
                return h_flex()
                    .size_full()
                    .child(
                        v_flex()
                            .w(ginka_ui::layout::PLACE_PANE_WIDTH)
                            .h_full()
                            .flex_shrink_0()
                            .bg(tokens.colors().bg_sidebar)
                            .border_r_1()
                            .border_color(tokens.colors().border_subtle)
                            .child(super::header(cx))
                            .child(super::connections(false, cx)),
                    )
                    .child(div().flex_1().h_full().child(self.sign_in.clone()))
                    .into_any_element();
            }
            let centre = if self.showing {
                div()
                    .size_full()
                    .child(self.detail.clone())
                    .into_any_element()
            } else {
                super::empty(rust_i18n::t!("inbox.pick").to_string(), cx).into_any_element()
            };
            h_flex()
                .size_full()
                .child(
                    div()
                        .w(ginka_ui::layout::PLACE_PANE_WIDTH)
                        .h_full()
                        .flex_shrink_0()
                        .child(self.list_column(cx)),
                )
                .child(div().flex_1().min_w_0().h_full().child(centre))
                .when(self.asking, |this| {
                    this.child(
                        div()
                            .w(ginka_ui::layout::PLACE_PANE_WIDTH)
                            .h_full()
                            .flex_shrink_0()
                            .border_l_1()
                            .border_color(tokens.colors().border_subtle)
                            .child(self.agent.clone()),
                    )
                })
                .into_any_element()
        }
    }
}

#[cfg(feature = "github")]
pub use mounted::{Inbox, InboxUnread};

/// The list column's header: what this place is.
fn header(cx: &App) -> impl IntoElement + use<> {
    let tokens = Tokens::global(cx);
    h_flex()
        .h(HEADER_HEIGHT)
        .flex_shrink_0()
        .px_4()
        .gap_2()
        .items_center()
        .child(
            Icon::new(IconName::Inbox)
                .size_4()
                .text_color(tokens.colors().text_secondary),
        )
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tokens.colors().text_primary)
                .child(rust_i18n::t!("nav.inbox").to_string()),
        )
}

/// The connections across the top of the list: GitHub, and the way to add
/// another — which says, for now, that GitHub is the one there is.
fn connections(connected: bool, cx: &App) -> impl IntoElement + use<> {
    let tokens = Tokens::global(cx);
    h_flex()
        .w_full()
        .px_3()
        .pb_2()
        .gap_1()
        .child(
            h_flex()
                .px_2p5()
                .py_1()
                .gap_1p5()
                .items_center()
                .rounded(px(tokens.radius.row))
                .when(connected, |this| this.bg(tokens.colors().row_active()))
                .child(
                    Icon::new(IconName::Github)
                        .size_3p5()
                        .text_color(tokens.colors().text_primary),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(tokens.colors().text_primary)
                        .child("GitHub"),
                ),
        )
        .child(
            h_flex()
                .id("inbox-add-connection")
                .px_2p5()
                .py_1()
                .gap_1p5()
                .items_center()
                .rounded(px(tokens.radius.row))
                .tooltip(|window, cx| {
                    Tooltip::new(rust_i18n::t!("inbox.add_connection.later").to_string())
                        .build(window, cx)
                })
                .child(
                    Icon::new(IconName::Plus)
                        .size_3p5()
                        .text_color(tokens.colors().text_muted),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("inbox.add_connection").to_string()),
                ),
        )
}

/// A centred line, for a column with nothing chosen in it.
#[cfg(feature = "github")]
fn empty(text: String, cx: &App) -> impl IntoElement + use<> {
    let tokens = Tokens::global(cx);
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .px_8()
        .child(
            div()
                .text_size(px(13.))
                .text_color(tokens.colors().text_muted)
                .child(text),
        )
}

/// The Inbox in a build without the GitHub client: what it would be, and the
/// command that builds it in.
#[cfg(not(feature = "github"))]
pub fn unavailable(cx: &App) -> impl IntoElement + use<> {
    let tokens = Tokens::global(cx).clone();
    h_flex()
        .size_full()
        .child(
            v_flex()
                .w(ginka_ui::layout::PLACE_PANE_WIDTH)
                .h_full()
                .flex_shrink_0()
                .bg(tokens.colors().bg_sidebar)
                .border_r_1()
                .border_color(tokens.colors().border_subtle)
                .child(header(cx))
                .child(connections(false, cx)),
        )
        .child(
            v_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .gap_3()
                .px_8()
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(tokens.colors().text_secondary)
                        .child(rust_i18n::t!("inbox.unavailable").to_string()),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(px(tokens.radius.control()))
                        .bg(tokens.colors().code_bg)
                        .text_size(px(12.))
                        .text_color(tokens.colors().text_secondary)
                        .child("cargo run --features github"),
                ),
        )
}
