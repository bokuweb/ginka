//! The window shell: title bar plus the three resizable columns of
//! `docs/ui.md` §1.
//!
//! The centre column carries the transcript, the composer, the context bar and
//! the terminal dock. In M0 the transcript and terminal are placeholders — the
//! point of this milestone is that the *layout* is right and the theme reaches
//! every corner of it before there is any content to argue about.

use crate::sidebar::SessionSidebar;
use crate::surfaces::SurfacePanel;
use ginka_core::settings::AppSettings;
use ginka_ui::Tokens;
use ginka_ui::workspace::SessionRow;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    Icon, IconName, StyledExt as _, TitleBar, h_flex,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};

/// Width macOS reserves for the traffic lights before our own content starts.
const TRAFFIC_LIGHT_INSET: Pixels = px(78.);

pub struct Shell {
    settings: AppSettings,
    /// The row the centre column is showing.
    session: SessionRow,
    sidebar: Entity<SessionSidebar>,
    surfaces: Entity<SurfacePanel>,
    /// Dropping this stops the app following the system appearance.
    _appearance: Subscription,
}

impl Shell {
    pub fn new(settings: AppSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
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

        let rows = SessionRow::samples();
        // The centre column shows whichever row the sidebar has selected; until
        // selection is wired in M1 that is simply the first.
        let session = rows[0].clone();
        let sidebar = cx.new(|_| SessionSidebar::new(rows));
        let surfaces = cx.new(|_| SurfacePanel::new());
        Self {
            settings,
            session,
            sidebar,
            surfaces,
            _appearance: appearance,
        }
    }

    /// Three regions, aligned to the columns beneath: window controls over the
    /// sidebar, the session's identity over the transcript, surface controls
    /// over the right panel. `docs/ui.md` §3.1.
    fn title_bar(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let muted = tokens.colors().text_muted;
        let secondary = tokens.colors().text_secondary;
        // macOS already reserves the leading inset for the traffic lights.
        let nav_width = px(self.settings.sidebar_width) - TRAFFIC_LIGHT_INSET;

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
                        .child(self.session.agent.glyph().size_4().text_color(secondary))
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(tokens.colors().text_primary)
                                .child(self.session.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(self.session.origin.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(Icon::new(IconName::Plus).size_4().text_color(secondary))
                        .child(Icon::new(IconName::Maximize).size_4().text_color(muted))
                        .child(Icon::new(IconName::PanelRight).size_4().text_color(muted)),
                ),
        )
    }

    /// Placeholder transcript. M2 replaces this with the streaming event views.
    fn transcript(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
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
                    .child(
                        "The shell is in place: a session list on the left, the transcript and \
                         composer here, surfaces on the right, and a terminal dock below. \
                         Streaming agent output lands in M2.",
                    ),
            )
            .child(
                div()
                    .max_w(px(720.))
                    .text_size(px(15.))
                    .line_height(px(25.))
                    .text_color(tokens.colors().text_secondary)
                    .child(
                        "Resize the sidebar and the right panel to check the columns hold their \
                         ranges, and switch the system appearance to check the tokens reach \
                         every surface.",
                    ),
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
            .child(
                self.session
                    .agent
                    .glyph()
                    .size_3()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_secondary)
                    .child(self.session.agent.label()),
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
                            .child(self.session.branch.clone()),
                    ),
            )
    }

    /// Placeholder terminal dock. M3 replaces the body with a real PTY grid.
    fn terminal_dock(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .w_full()
            .h(px(200.))
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
                        div()
                            .text_color(tokens.colors().accent)
                            .child("~/.ginka/worktrees/ginka/bright-harbor"),
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

    fn center(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(self.transcript(cx))
            .child(self.composer(cx))
            .when(self.settings.terminal_dock_open, |this| {
                this.child(self.terminal_dock(cx))
            })
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
        let bg = tokens.colors().bg_window;
        let sidebar_width = px(self.settings.sidebar_width);
        let right_width = px(self.settings.right_panel_width);
        let right_open = self.settings.right_panel_open;

        v_flex()
            .size_full()
            .bg(bg)
            .text_color(tokens.colors().text_primary)
            .child(self.title_bar(cx))
            .child(
                div().flex_1().w_full().overflow_hidden().child(
                    h_resizable("shell-columns")
                        .child(
                            resizable_panel()
                                .size(sidebar_width)
                                .size_range(px(200.)..px(400.))
                                .child(self.sidebar.clone()),
                        )
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
