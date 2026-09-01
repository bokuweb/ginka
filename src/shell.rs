//! The window shell: title bar plus the three resizable columns of
//! `docs/ui.md` §1.
//!
//! The centre column carries the transcript, the composer, the context bar and
//! the terminal dock. In M0 the transcript and terminal are placeholders — the
//! point of this milestone is that the *layout* is right and the theme reaches
//! every corner of it before there is any content to argue about.

use crate::sidebar::{SessionSidebar, SidebarEvent};
use crate::surfaces::SurfacePanel;
use ginka_core::Paths;
use ginka_core::settings::{self, AppSettings};
use ginka_ui::Tokens;
use ginka_ui::layout::{Layout, Panel};
use ginka_ui::workspace::SessionRow;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    Icon, IconName, StyledExt as _, TitleBar, h_flex,
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    v_flex,
};
use std::time::Duration;

actions!(shell, [ToggleSidebar, ToggleRightPanel, ToggleTerminalDock]);

const CONTEXT: &str = "Shell";

/// Bind the panel toggles.
///
/// The chords follow VS Code, because that is the muscle memory everyone using
/// this app already has.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-b", ToggleSidebar, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-b", ToggleSidebar, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-b", ToggleRightPanel, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-b", ToggleRightPanel, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-j", ToggleTerminalDock, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-j", ToggleTerminalDock, Some(CONTEXT)),
    ]);
}

/// How often worktrees and their git status are re-read.
///
/// Matches the daemon's planned sync cadence (`DaemonSettings::sync_interval_secs`).
/// When the daemon owns this in M2 the UI will be told rather than polling.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// Width macOS reserves for the traffic lights before our own content starts.
const TRAFFIC_LIGHT_INSET: Pixels = px(78.);

pub struct Shell {
    /// Where `app.json` lives, so a toggle can be written straight back.
    paths: Paths,
    settings: AppSettings,
    layout: Layout,
    /// The row the centre column is showing. `None` when nothing is
    /// registered, which is the first-run state rather than an error.
    session: Option<SessionRow>,
    sidebar: Entity<SessionSidebar>,
    surfaces: Entity<SurfacePanel>,
    /// Dropping these stops the app following the system appearance and the
    /// sidebar's selection.
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(
        paths: Paths,
        settings: AppSettings,
        rows: Vec<SessionRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The window knows the real system appearance; the App-level default
        // applied at startup was a guess made before any window existed.
        ginka_ui::theme::apply(
            ginka_ui::Mode::resolve(settings.appearance, window.appearance()),
            cx,
        );

        let choice = settings.appearance;
        let appearance = window.observe_window_appearance(move |window, cx| {
            ginka_ui::theme::apply(ginka_ui::Mode::resolve(choice, window.appearance()), cx);
            window.refresh();
        });

        // The centre column shows whichever row the sidebar has selected; until
        // selection is wired up that is simply the first.
        let session = rows.first().cloned();
        let sidebar = cx.new(|_| SessionSidebar::new(rows));
        let surfaces = cx.new(|_| SurfacePanel::new());

        let selection = cx.subscribe(&sidebar, |this, sidebar, event, cx| match event {
            SidebarEvent::Selected => {
                this.session = sidebar.read(cx).selected_row().cloned();
                cx.notify();
            }
        });

        // Worktrees change outside the app -- an agent commits, the user
        // switches a branch in a terminal, someone runs `git worktree add`. A
        // tick is how those reach the window without the user reopening it.
        cx.spawn({
            let paths = paths.clone();
            async move |this, cx| {
                loop {
                    cx.background_executor().timer(REFRESH_INTERVAL).await;
                    let paths = paths.clone();
                    // Storage and one `git status` per worktree: off the main
                    // thread, or the window stalls every tick.
                    let rows = cx
                        .background_spawn(async move { crate::sessions::load(&paths) })
                        .await;
                    tracing::debug!(rows = rows.len(), "refreshed sessions");
                    let updated = this.update(cx, |this, cx| {
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.set_rows(rows, cx));
                        this.session = this.sidebar.read(cx).selected_row().cloned();
                        cx.notify();
                    });
                    if updated.is_err() {
                        // The window is gone; stop ticking.
                        break;
                    }
                }
            }
        })
        .detach();
        Self {
            layout: Layout::from_settings(&settings),
            paths,
            settings,
            session,
            sidebar,
            surfaces,
            _subscriptions: vec![appearance, selection],
        }
    }

    fn toggle(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.layout.toggle(panel);
        self.persist();
        cx.notify();
    }

    /// Write the layout back to `app.json`.
    ///
    /// Synchronous and immediate: the file is small and the write is atomic, and
    /// an arrangement that survives a crash is worth more than the microseconds.
    /// If this ever shows up in a profile, debounce it -- do not move it off the
    /// toggle, or the state stops matching what the user sees.
    fn persist(&mut self) {
        self.layout.write_into(&mut self.settings);
        if let Err(error) = settings::save(&self.paths.app_settings(), &self.settings) {
            tracing::warn!(%error, "could not persist the panel layout");
        }
    }

    /// Store the sizes a divider drag produced.
    ///
    /// The slot map comes from the layout, not from a fixed index: with the
    /// sidebar closed, index 0 is the centre column.
    fn record_resize(
        &mut self,
        slots: Vec<Option<Panel>>,
        state: &Entity<ResizableState>,
        cx: &mut App,
    ) {
        let sizes = state.read(cx).sizes().clone();
        self.layout.record_sizes(&slots, &sizes);
        self.persist();
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle(Panel::Sidebar, cx);
    }

    fn on_toggle_right_panel(
        &mut self,
        _: &ToggleRightPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle(Panel::RightPanel, cx);
    }

    fn on_toggle_terminal_dock(
        &mut self,
        _: &ToggleTerminalDock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle(Panel::TerminalDock, cx);
    }

    /// A title-bar control that opens or closes a panel.
    ///
    /// The icon reports the state rather than the action -- an open panel shows
    /// the "close" variant -- which is what VS Code does and what makes the
    /// control readable without hovering it.
    fn panel_toggle(
        &self,
        panel: Panel,
        open_icon: IconName,
        closed_icon: IconName,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let open = self.layout.is_open(panel);
        let color = if open {
            tokens.colors().text_secondary
        } else {
            tokens.colors().text_muted
        };
        let radius = px(tokens.radius.row);
        let hover_bg = tokens.colors().bg_raised;

        div()
            .id(SharedString::from(format!("toggle-{}", panel.label())))
            .p_1()
            .rounded(radius)
            .hover(move |this| this.bg(hover_bg))
            .child(
                Icon::new(if open { open_icon } else { closed_icon })
                    .size_4()
                    .text_color(color),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(panel, cx)))
    }

    /// Three regions, aligned to the columns beneath: window controls over the
    /// sidebar, the session's identity over the transcript, surface controls
    /// over the right panel. `docs/ui.md` §3.1.
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let muted = tokens.colors().text_muted;
        let secondary = tokens.colors().text_secondary;
        let primary = tokens.colors().text_primary;
        let session = self.session.clone();
        let title: SharedString = session
            .as_ref()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "Ginka".into());
        let origin: SharedString = session
            .as_ref()
            .map(|session| session.origin.clone())
            .unwrap_or_else(|| "no project registered".into());
        let glyph = session.as_ref().map(|session| session.agent.glyph());
        // Track the column beneath. macOS already reserves the leading inset for
        // the traffic lights; with the sidebar closed there is no column to
        // align to, so the controls sit directly after them.
        let nav_width = if self.layout.is_open(Panel::Sidebar) {
            self.layout.size(Panel::Sidebar) - TRAFFIC_LIGHT_INSET
        } else {
            px(0.)
        };

        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_3()
                .items_center()
                .child(
                    h_flex()
                        .w(nav_width)
                        .gap_3()
                        .items_center()
                        .child(
                            Icon::new(IconName::PanelLeft)
                                .size_4()
                                .text_color(secondary),
                        )
                        .child(Icon::new(IconName::ArrowLeft).size_4().text_color(muted))
                        .child(Icon::new(IconName::ArrowRight).size_4().text_color(muted)),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .gap_2()
                        .items_center()
                        .overflow_hidden()
                        .children(glyph.map(|glyph| glyph.size_4().text_color(secondary)))
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(primary)
                                .child(title),
                        )
                        .child(div().text_xs().text_color(muted).truncate().child(origin)),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(Icon::new(IconName::Plus).size_4().text_color(secondary))
                        .child(self.panel_toggle(
                            Panel::TerminalDock,
                            IconName::PanelBottom,
                            IconName::PanelBottomOpen,
                            cx,
                        ))
                        .child(self.panel_toggle(
                            Panel::RightPanel,
                            IconName::PanelRightClose,
                            IconName::PanelRightOpen,
                            cx,
                        )),
                ),
        )
    }

    /// Placeholder transcript. M2 replaces this with the streaming event views.
    fn transcript(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let body = match &self.session {
            Some(session) => format!(
                "{} is checked out on {}. Streaming agent output lands in M2.",
                session.title, session.branch
            ),
            None => "No project is registered yet. Run `ginka project add .` in a repository, \
                     then reopen this window."
                .to_string(),
        };

        v_flex()
            .id("transcript")
            .flex_1()
            .px_8()
            .py_6()
            .gap_4()
            .overflow_y_scroll()
            .child(
                div()
                    // The measure from docs/ui.md: long-form text stays
                    // readable because the column stops growing, not because
                    // the window does.
                    .max_w(px(720.))
                    .text_size(px(15.))
                    .line_height(px(25.))
                    .text_color(tokens.colors().text_primary)
                    .child(body),
            )
    }

    fn composer(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .w_full()
            .px_4()
            .pb_3()
            .gap_1p5()
            .child(
                h_flex()
                    .w_full()
                    .px_3p5()
                    .py_3()
                    .gap_3()
                    .items_center()
                    .rounded(px(tokens.radius.panel))
                    .bg(tokens.colors().bg_surface)
                    .border_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(tokens.colors().text_muted)
                            .child("Do anything…"),
                    )
                    .child(self.model_chip(cx))
                    .child(self.chip("Agent", cx))
                    .child(
                        Icon::empty()
                            .path(ginka_ui::assets::icon::PAPERCLIP)
                            .size_4()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        div()
                            .size_7()
                            .rounded_full()
                            .bg(tokens.colors().text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::ArrowUp)
                                    .size_4()
                                    .text_color(tokens.colors().bg_window),
                            ),
                    ),
            )
            .child(self.context_bar(cx))
    }

    /// The model picker carries the agent's glyph so the row reads as
    /// "which agent, which model" at a glance.
    fn model_chip(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .px_2()
            .py_0p5()
            .gap_1p5()
            .items_center()
            .rounded_full()
            .bg(tokens.colors().bg_raised)
            .children(self.session.as_ref().map(|session| {
                session
                    .agent
                    .glyph()
                    .size_3()
                    .text_color(tokens.colors().text_secondary)
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_secondary)
                    .child(
                        self.session
                            .as_ref()
                            .map(|session| session.agent.label())
                            .unwrap_or("No agent"),
                    ),
            )
    }

    fn chip(&self, label: &'static str, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        div()
            .px_2()
            .py_0p5()
            .rounded_full()
            .bg(tokens.colors().bg_raised)
            .text_xs()
            .text_color(tokens.colors().text_secondary)
            .child(label)
    }

    /// The hairline strip under the composer: worktree left, branch right.
    fn context_bar(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_1()
            .justify_between()
            .items_center()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::Folder)
                            .size_3()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child("Worktree"),
                    ),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::empty()
                            .path(ginka_ui::assets::icon::GIT_BRANCH)
                            .size_3()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(
                                self.session
                                    .as_ref()
                                    .map(|session| session.branch.clone())
                                    .unwrap_or_else(|| "—".into()),
                            ),
                    ),
            )
    }

    /// Placeholder terminal dock. M3 replaces the body with a real PTY grid.
    fn terminal_dock(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .size_full()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
            .bg(tokens.colors().bg_terminal)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .gap_2()
                            .items_center()
                            .rounded(px(tokens.radius.row))
                            .bg(tokens.colors().bg_raised)
                            .child(
                                Icon::new(IconName::SquareTerminal)
                                    .size_3()
                                    .text_color(tokens.colors().text_secondary),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.colors().text_secondary)
                                    .child("local"),
                            )
                            .child(
                                Icon::new(IconName::Close)
                                    .size_3()
                                    .text_color(tokens.colors().text_muted),
                            ),
                    )
                    .child(
                        Icon::new(IconName::Plus)
                            .size_3p5()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(div().flex_1())
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(tokens.colors().text_muted),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .px_3()
                    .py_2()
                    .items_center()
                    .font_family(cx.theme_mono_font())
                    .text_size(px(13.))
                    // A real prompt is not one colour. Until the PTY lands in
                    // M3, the placeholder at least has the right shape.
                    .child(
                        div()
                            .text_color(tokens.colors().status_done)
                            .child("ginka@local"),
                    )
                    .child(div().text_color(tokens.colors().text_muted).child(":"))
                    .child(
                        div().text_color(tokens.colors().accent).child(
                            // The real worktree, not a stand-in: a prompt that
                            // names a directory the user is not in is worse than
                            // no prompt.
                            self.session
                                .as_ref()
                                .map(|session| session.path.display().to_string())
                                .unwrap_or_else(|| "~".to_string()),
                        ),
                    )
                    .child(div().text_color(tokens.colors().text_muted).child("$"))
                    .child(
                        div()
                            .ml_1p5()
                            .w(px(7.))
                            .h(px(15.))
                            .bg(tokens.colors().text_secondary),
                    ),
            )
    }

    fn center(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dock_open = self.layout.is_open(Panel::TerminalDock);
        let dock_height = self.layout.size(Panel::TerminalDock);

        v_flex().size_full().child(
            v_resizable("centre-rows")
                .on_resize({
                    let this = cx.entity();
                    let slots = self.layout.rows();
                    move |state, _, cx| {
                        let slots = slots.clone();
                        let state = state.clone();
                        this.update(cx, |this, cx| this.record_resize(slots, &state, cx));
                    }
                })
                .child(
                    resizable_panel().child(
                        v_flex()
                            .size_full()
                            .child(self.transcript(cx))
                            .child(self.composer(cx))
                            .into_any_element(),
                    ),
                )
                .when(dock_open, |this| {
                    this.child(
                        resizable_panel()
                            .size(dock_height)
                            .size_range(px(120.)..px(560.))
                            .child(self.terminal_dock(cx).into_any_element()),
                    )
                }),
        )
    }
}

/// The mono family is a theme concern, not a per-view constant.
trait MonoFont {
    fn theme_mono_font(&self) -> SharedString;
}

impl MonoFont for App {
    fn theme_mono_font(&self) -> SharedString {
        gpui_component::Theme::global(self).mono_font_family.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let sidebar_open = self.layout.is_open(Panel::Sidebar);
        let right_open = self.layout.is_open(Panel::RightPanel);
        let sidebar_width = self.layout.size(Panel::Sidebar);
        let right_width = self.layout.size(Panel::RightPanel);

        v_flex()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_right_panel))
            .on_action(cx.listener(Self::on_toggle_terminal_dock))
            .size_full()
            // No background here: `Root` already paints the translucent window
            // and painting it again composites the alpha away.
            .text_color(tokens.colors().text_primary)
            .child(self.title_bar(cx))
            .child(
                div().flex_1().w_full().overflow_hidden().child(
                    h_resizable("shell-columns")
                        .on_resize({
                            let this = cx.entity();
                            let slots = self.layout.columns();
                            move |state, _, cx| {
                                let slots = slots.clone();
                                let state = state.clone();
                                this.update(cx, |this, cx| this.record_resize(slots, &state, cx));
                            }
                        })
                        .when(sidebar_open, |this| {
                            this.child(
                                resizable_panel()
                                    .size(sidebar_width)
                                    .size_range(px(200.)..px(400.))
                                    .child(self.sidebar.clone()),
                            )
                        })
                        .child(resizable_panel().child(self.center(cx).into_any_element()))
                        .when(right_open, |this| {
                            this.child(
                                resizable_panel()
                                    .size(right_width)
                                    .size_range(px(280.)..px(720.))
                                    .child(self.surfaces.clone()),
                            )
                        }),
                ),
            )
    }
}
