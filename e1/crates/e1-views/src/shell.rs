//! The standalone window: three resizable columns under three header strips,
//! and no title bar (`docs/ui.md` §3.1).
//!
//! This is the one view a host will not mount: it owns the window's controls,
//! the arrangement and its persistence, all of which are the host's when the
//! views are embedded. Everything under it — the sidebar, the list, the
//! detail — is mounted here exactly the way a host would mount it.

use crate::agent::{AgentPane, AgentPaneEvent};
use crate::browser::{BrowserEvent, FileBrowser};
use crate::detail::{Detail, DetailEvent};
use crate::history::{History, HistoryEvent};
use crate::list::{ItemEvent, ItemList};
use crate::palette::{Palette, PaletteEvent};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::signin::{SignIn, SignInEvent};
use crate::store::{ItemKey, Store, StoreEvent};
use e1_github::auth::{Keychain, Source};
use e1_github::{GitHub, HttpCache, Rest, Scripted, StatusFilter};
use e1_ui::Mode;
use e1_ui::palette::Pick;
use e1_ui::settings::{self, AppSettings, Appearance};
use e1_ui::theme::ThemeAppearance;
use e1_ui::{
    Focus, HEADER_HEIGHT, Layout, Panel, Paths, RepoTab, SidebarResizeLimits, TRAFFIC_LIGHT_INSET,
    Tokens,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    Icon, IconName, InteractiveElementExt as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use std::sync::Arc;
use std::time::Instant;

actions!(e1, [ToggleSidebar, ToggleRightPanel, Refresh, OpenPalette]);

/// The key context the shell's chords are bound in.
const CONTEXT: &str = "E1Shell";

/// How wide a column's grab area is, centred on its edge.
const HANDLE_WIDTH: Pixels = px(9.);

/// The narrowest the centre column is let get.
const CENTRE_MIN: Pixels = px(320.);

/// A divider being dragged.
#[derive(Debug, Clone, Copy)]
struct ColumnDrag {
    /// Which column is being resized.
    panel: Panel,
    /// Where the press was.
    start_x: Pixels,
    /// The arrangement at the press, used to restore coupled widths when a
    /// drag reverses direction.
    start_layout: Layout,
}

/// A column on its way open or closed.
#[derive(Debug, Clone, Copy)]
struct Transition {
    /// When it started.
    began: Instant,
    /// Whether it is opening (`true`) or closing.
    opening: bool,
}

/// Bind the panel toggles and refresh.
///
/// The chords follow VS Code, because that is the muscle memory everyone
/// using this app already has.
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
        KeyBinding::new("cmd-r", Refresh, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-r", Refresh, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", OpenPalette, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-k", OpenPalette, Some(CONTEXT)),
    ]);
}

/// The window.
pub struct Shell {
    paths: Paths,
    settings: AppSettings,
    layout: Layout,
    store: Entity<Store>,
    sidebar: Entity<Sidebar>,
    list: Entity<ItemList>,
    detail: Entity<Detail>,
    /// The CLI-backed conversation at the far right.
    agent: Entity<AgentPane>,
    browser: Entity<FileBrowser>,
    /// The centre column as a repository's commits.
    history: Entity<History>,
    sign_in: Entity<SignIn>,
    /// Everywhere the window can go, behind ⌘K.
    palette: Entity<Palette>,
    /// What the centre column was last pointed at.
    current: Option<Focus>,
    /// The appearance changed and the theme has to be installed at the next
    /// frame, which is the first place with a window to ask.
    retheme: bool,
    /// The divider under the pointer, while one is.
    resizing: Option<ColumnDrag>,
    /// Columns mid-way through opening or closing, by panel.
    transitions: Vec<(Panel, Transition)>,
    /// Whether there is a GitHub to draw. Without one the centre column is
    /// the sign-in screen.
    signed_in: bool,
    /// Where the token came from, which is what signing out has to undo.
    token_source: Option<Source>,
    focus_handle: FocusHandle,
    /// With no title bar the strips are what the window is dragged by, and a
    /// drag is a press that then moved. Set on the press, cleared on the
    /// release, acted on by the first move in between.
    dragging: bool,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// Open over a source, with the arrangement the settings remember.
    ///
    /// `github` is `None` when no token was found: the window opens on the
    /// sign-in screen and everything else waits. `token_source` says where
    /// a token came from, so signing out knows whether it can delete it.
    pub fn new(
        github: Option<Arc<dyn GitHub>>,
        token_source: Option<Source>,
        paths: Paths,
        settings: AppSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let signed_in = github.is_some();
        let source: Arc<dyn GitHub> = github.unwrap_or_else(|| Arc::new(Scripted::empty()));
        let snapshot = paths.snapshot();
        let avatars = paths.cache().join("avatars");
        let store = cx.new(|_| {
            let store = Store::new(source).with_avatars(avatars);
            // A window that opens signed out must not read a snapshot that
            // belongs to whoever was signed in before.
            if signed_in {
                store.with_snapshot(snapshot)
            } else {
                store.remembering(snapshot)
            }
        });
        let sign_in = cx.new(|_| SignIn::new());
        let browser = cx.new(|cx| FileBrowser::new(store.clone(), window, cx));
        let history = cx.new(|cx| History::new(store.clone(), cx));
        let palette = cx.new(|cx| Palette::new(store.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(store.clone(), cx));
        let list = cx.new(|cx| ItemList::new(store.clone(), cx));
        let detail = cx.new(|cx| Detail::new(store.clone(), window, cx));
        let agent = cx.new(|cx| AgentPane::new(store.clone(), window, cx));

        let mut subscriptions = Vec::new();
        let agent_for_ask = agent.clone();
        subscriptions.push(
            cx.subscribe(&detail, move |this, _, event, cx| match event {
                DetailEvent::Ask(ask) => {
                    tracing::info!("opening agent pane from Ask AI");
                    agent_for_ask.update(cx, |agent, cx| agent.open(ask.clone(), cx));
                    if !this.layout.is_open(Panel::AgentPanel) {
                        this.toggle(Panel::AgentPanel, cx);
                    }
                }
            }),
        );
        subscriptions.push(cx.subscribe(&agent, |this, _, event, cx| match event {
            AgentPaneEvent::Close => {
                if this.layout.is_open(Panel::AgentPanel) {
                    this.toggle(Panel::AgentPanel, cx);
                }
            }
            AgentPaneEvent::AgentChosen(kind) => {
                this.settings.agent = Some(kind.id().to_string());
                this.persist();
            }
            AgentPaneEvent::Tuned(kind, tuning) => {
                // Nothing chosen is a choice too — it is how a reader goes
                // back to the CLI's own default — so it removes the entry
                // rather than writing an empty one.
                let keep = |field: &mut std::collections::BTreeMap<String, String>,
                            value: Option<&str>| match value {
                    Some(value) => {
                        field.insert(kind.id().to_string(), value.to_string());
                    }
                    None => {
                        field.remove(kind.id());
                    }
                };
                keep(&mut this.settings.agent_models, tuning.model());
                keep(&mut this.settings.agent_efforts, tuning.effort());
                this.persist();
            }
        }));
        subscriptions.push(cx.subscribe(&history, |this, _, event, cx| match event {
            HistoryEvent::Open { repo, sha } => {
                let (repo, sha) = (repo.clone(), sha.clone());
                this.detail
                    .update(cx, |detail, cx| detail.show_commit(repo, sha, cx));
                if !this.layout.is_open(Panel::RightPanel) {
                    this.toggle(Panel::RightPanel, cx);
                }
            }
        }));
        subscriptions.push(cx.subscribe(&browser, |this, _, event, cx| match event {
            BrowserEvent::Open { repo, path } => {
                let key = (repo.clone(), path.clone());
                this.detail
                    .update(cx, |detail, cx| detail.show_file(key, cx));
                if !this.layout.is_open(Panel::RightPanel) {
                    this.toggle(Panel::RightPanel, cx);
                }
            }
        }));
        subscriptions.push(cx.subscribe(&palette, |this, _, event, cx| match event {
            PaletteEvent::Pick(pick) => this.go(pick.clone(), cx),
        }));
        subscriptions.push(cx.subscribe(&sidebar, |this, _, event, cx| match event {
            SidebarEvent::Focus(focus) => this.focus_on(focus.clone(), cx),
            SidebarEvent::SignOut => this.sign_out(cx),
            SidebarEvent::ToggleAppearance => this.toggle_appearance(cx),
            SidebarEvent::OwnerToggled { owner, collapsed } => {
                this.settings.collapsed_owners.retain(|o| o != owner);
                if *collapsed {
                    this.settings.collapsed_owners.push(owner.clone());
                }
                this.persist();
            }
        }));
        // The OS can change its appearance while the window is open; when
        // the setting is to follow it, the window follows.
        subscriptions.push(window.observe_window_appearance({
            let this = cx.entity().downgrade();
            move |window, cx| {
                this.update(cx, |this, cx| this.apply_theme(window, cx))
                    .ok();
            }
        }));
        subscriptions.push(cx.subscribe(&sign_in, |this, _, event, cx| match event {
            SignInEvent::SignedIn(token) => {
                let cache = HttpCache::new(this.paths.http_cache());
                let github: Arc<dyn GitHub> = Arc::new(Rest::new(token.clone()).with_cache(cache));
                this.signed_in = true;
                this.token_source = Some(Source::Keychain);
                this.store
                    .update(cx, |store, cx| store.set_source(github, cx));
                this.open_inbox(cx);
                cx.notify();
            }
        }));
        subscriptions.push(cx.subscribe(&list, |this, _, event, cx| match event {
            ItemEvent::Open { key, is_pull } => {
                let (key, is_pull) = (key.clone(), *is_pull);
                this.detail
                    .update(cx, |detail, cx| detail.show(key, Some(is_pull), cx));
                // A row that was opened wants to be read; a closed reading
                // pane would make the click do nothing visible.
                if !this.layout.is_open(Panel::RightPanel) {
                    this.toggle(Panel::RightPanel, cx);
                }
            }
            ItemEvent::OpenProject(project) => {
                this.detail
                    .update(cx, |detail, cx| detail.show_project(project.clone(), cx));
                if !this.layout.is_open(Panel::RightPanel) {
                    this.toggle(Panel::RightPanel, cx);
                }
            }
            ItemEvent::OpenUrl(url) => cx.open_url(url),
        }));
        // The headers show what the store knows (a title, a spinner), so a
        // change there is a redraw here.
        subscriptions.push(cx.subscribe(&store, |_, _, _: &StoreEvent, cx| cx.notify()));

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_collapsed(settings.collapsed_owners.iter().cloned(), cx);
        });

        let layout = Layout::from_settings(&settings);
        let mut this = Self {
            paths,
            settings,
            layout,
            store,
            sidebar,
            list,
            detail,
            agent,
            browser,
            history,
            sign_in,
            palette,
            current: None,
            retheme: false,
            resizing: None,
            transitions: Vec::new(),
            signed_in,
            token_source,
            focus_handle,
            dragging: false,
            _subscriptions: subscriptions,
        };
        // Which agent CLIs are here does not depend on being signed in, and
        // the answer takes a second to find, so the looking starts now. The
        // one an ask goes to was remembered; whether it is still installed
        // is the store's question to answer.
        let chosen = this
            .settings
            .agent
            .as_deref()
            .and_then(e1_ui::agents::Kind::parse);
        let tuning: Vec<(e1_ui::agents::Kind, e1_ui::agents::Tuning)> = e1_ui::agents::Kind::ALL
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    e1_ui::agents::Tuning {
                        model: this.settings.agent_models.get(kind.id()).cloned(),
                        effort: this.settings.agent_efforts.get(kind.id()).cloned(),
                    },
                )
            })
            .filter(|(_, tuning)| tuning != &e1_ui::agents::Tuning::default())
            .collect();
        this.store.update(cx, |store, cx| {
            if let Some(kind) = chosen {
                store.choose_agent(kind, cx);
            }
            for (kind, tuning) in tuning {
                store.tune(kind, tuning, cx);
            }
            store.load_agents(cx);
        });
        if signed_in {
            this.store.update(cx, |store, cx| store.refresh_all(cx));
            // The window opens on the inbox, which is the question a person
            // opens GitHub to answer. Pointed directly rather than through
            // the sidebar's event, which would land after whatever the
            // caller does next and undo it.
            this.open_inbox(cx);
        }
        this
    }

    /// Open an item on its files as soon as the window is up.
    ///
    /// For demos and screenshots (`E1_DEMO_OPEN=owner/name#12:src/main.rs`,
    /// the path optional; `E1_DEMO_LOG=2` for a job's log on top): a native
    /// window cannot be driven from a script the way a page can, and a
    /// screenshot of the diff view is worth an environment variable.
    pub fn open_at_launch(
        &mut self,
        key: ItemKey,
        file: Option<String>,
        log: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        self.detail.update(cx, |detail, cx| {
            detail.show(key.clone(), None, cx);
            if file.is_some() {
                detail.show_files(file, cx);
            }
            if let Some(job) = log {
                detail.show_log(key.0, job, format!("job {job}"), cx);
            }
            if std::env::var_os("E1_DEMO_ASK").is_some() {
                detail.ask_at_launch(cx);
            }
        });
        if !self.layout.is_open(Panel::RightPanel) {
            self.toggle(Panel::RightPanel, cx);
        }
    }

    /// Open a repository's finder with a file read, as soon as the window is
    /// up. For screenshots (`E1_DEMO_FILES=owner/name:src/main.rs`).
    pub fn browse_at_launch(
        &mut self,
        repo: e1_github::RepoId,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.refocus(Focus::files(repo.clone()), cx);
        if let Some(path) = path {
            self.detail
                .update(cx, |detail, cx| detail.show_file((repo, path.clone()), cx));
            self.browser
                .update(cx, |browser, cx| browser.reveal(&path, cx));
        }
    }

    /// Open a repository's history with its newest commit read, as soon as
    /// the window is up. For screenshots (`E1_DEMO_HISTORY=owner/name`).
    pub fn history_at_launch(&mut self, repo: e1_github::RepoId, cx: &mut Context<Self>) {
        self.refocus(Focus::history(repo), cx);
        self.history
            .update(cx, |history, cx| history.open_newest(cx));
    }

    /// Forget the token and go back to the sign-in screen.
    ///
    /// Only a token this app stored is deleted. One from the environment or
    /// from `gh` is dropped for this session and found again next launch,
    /// because deleting it would be reaching into someone else's setup.
    fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.token_source == Some(Source::Keychain) {
            cx.background_spawn(async {
                if let Err(error) = Keychain::forget() {
                    tracing::warn!(%error, "could not delete the keychain entry");
                }
            })
            .detach();
        }
        self.signed_in = false;
        self.token_source = None;
        self.store.update(cx, |store, cx| {
            store.set_source(Arc::new(Scripted::empty()), cx)
        });
        cx.notify();
    }

    /// Point the centre column at something.
    fn focus_on(&mut self, focus: Focus, cx: &mut Context<Self>) {
        if let Some(repo) = focus.repo_id() {
            self.settings.last_repo = Some(repo.to_string());
            self.persist();
        }
        self.current = Some(focus.clone());
        match &focus {
            Focus::Files { repo } => {
                let repo = repo.clone();
                self.browser
                    .update(cx, |browser, cx| browser.set_repo(repo, cx));
            }
            Focus::History { repo } => {
                let repo = repo.clone();
                self.history
                    .update(cx, |history, cx| history.set_repo(repo, cx));
            }
            _ => self.list.update(cx, |list, cx| list.set_focus(focus, cx)),
        }
        cx.notify();
    }

    /// Light to dark and back.
    ///
    /// The flip is from what is on screen rather than from what is stored,
    /// because nothing may be stored: until the reader picks, the window is
    /// showing whatever the OS is, and a click has to take them off that in
    /// the direction they can see.
    fn toggle_appearance(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance = Some(match Tokens::global(cx).appearance {
            ThemeAppearance::Dark => Appearance::Light,
            ThemeAppearance::Light => Appearance::Dark,
        });
        self.persist();
        // Installing a theme needs a window, and a subscription callback has
        // none; the next frame does.
        self.retheme = true;
        cx.notify();
    }

    /// Install the theme the setting and the OS agree on, and redraw
    /// everything: the tokens are a global, so every view has to look again.
    ///
    /// Never call this while the window is drawing. The redraw is what makes
    /// the change whole, and it is dropped mid-draw.
    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = Mode::resolve(self.settings.appearance, window.appearance());
        e1_ui::theme::apply(mode, cx);
        window.refresh();
        cx.notify();
    }

    /// Point the window at the inbox, highlight and all.
    fn open_inbox(&mut self, cx: &mut Context<Self>) {
        let inbox = Focus::Section(e1_ui::Section::Inbox);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.adopt(inbox.clone(), cx));
        self.focus_on(inbox, cx);
    }

    /// Search GitHub for what was typed.
    fn search_for(&mut self, query: String, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| sidebar.clear(cx));
        self.focus_on(Focus::search(query), cx);
    }

    /// Go where a palette row points. A section and a repository are the
    /// same jump the sidebar makes, highlight and all; the search row is the
    /// one that leaves for the network.
    fn go(&mut self, pick: Pick, cx: &mut Context<Self>) {
        match pick {
            Pick::Section(section) => self.refocus(Focus::Section(section), cx),
            Pick::Repo(repo) => self.refocus(Focus::repo(repo), cx),
            Pick::Search(query) => self.search_for(query, cx),
        }
    }

    /// Open the palette over the window, empty and ready to type in.
    ///
    /// Nothing to jump to before there is a viewer, so a signed-out window
    /// answers ⌘K with nothing rather than with an empty box.
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.signed_in {
            return;
        }
        self.palette
            .update(cx, |palette, cx| palette.reset(window, cx));
        let palette = self.palette.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .w(px(560.))
                // High enough to sit under the pointer's own reach and low
                // enough to leave the window's top strip visible.
                .margin_top(px(96.))
                // Its own frame is the whole of it: no title, no close
                // button, and no padding, so the rule under the field runs
                // the full width the way a palette's does.
                .close_button(false)
                .p_0()
                // Opaque: the window's glass under a list of rows is
                // unreadable.
                .bg(Tokens::global(cx).colors().popover())
                // Use the dialog's content slot rather than its generic child
                // list. A `Command` is an entity-backed component; inside the
                // generic scrolling body its flex height resolves to zero, so
                // only the backdrop is painted. This is the integration shape
                // used by the toolkit's own command-dialog example.
                .content({
                    let palette = palette.clone();
                    move |content, _, _| content.child(palette.clone())
                })
        });
        // The dialog takes the focus for itself as it opens, so the caret
        // goes into the field once that has happened.
        let palette = self.palette.clone();
        window.defer(cx, move |window, cx| {
            let field = palette.read(cx).focus_handle(cx);
            field.focus(window, cx);
        });
    }

    /// What the centre column is showing, whichever view is showing it.
    fn focus(&self, cx: &App) -> Option<Focus> {
        // The file tree and the history carry their own repository, and one
        // of them is the focus while it is what is on screen; otherwise the
        // focus is the list's.
        if self.centre_is_browser(cx) {
            return self.browser_focus(cx);
        }
        if self.centre_is_history(cx) {
            return self.history_focus(cx);
        }
        self.list.read(cx).focus().cloned()
    }

    fn browser_focus(&self, cx: &App) -> Option<Focus> {
        self.browser.read(cx).repo().cloned().map(Focus::files)
    }

    fn history_focus(&self, cx: &App) -> Option<Focus> {
        self.history.read(cx).repo().cloned().map(Focus::history)
    }

    /// Whether the file tree is the centre column right now.
    fn centre_is_browser(&self, cx: &App) -> bool {
        matches!(self.current.as_ref(), Some(Focus::Files { .. }))
            && self.browser.read(cx).repo().is_some()
    }

    /// Whether the history is the centre column right now.
    fn centre_is_history(&self, cx: &App) -> bool {
        matches!(self.current.as_ref(), Some(Focus::History { .. }))
            && self.history.read(cx).repo().is_some()
    }

    /// Change the list without going through the sidebar: the kind and
    /// status toggles in the centre strip.
    fn refocus(&mut self, focus: Focus, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.adopt(focus.clone(), cx));
        self.focus_on(focus, cx);
    }

    fn toggle(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.layout.toggle(panel);
        self.persist();
        // The column slides rather than appearing: a panel that pops into
        // place is a layout jump, and one that slides is a thing moving.
        self.transitions.retain(|(other, _)| *other != panel);
        self.transitions.push((
            panel,
            Transition {
                began: Instant::now(),
                opening: self.layout.is_open(panel),
            },
        ));
        cx.notify();
    }

    /// How wide a column is drawn right now: its width, or a fraction of it
    /// while it slides. `None` when it is closed and not sliding.
    fn drawn_width(
        &mut self,
        panel: Panel,
        standard: std::time::Duration,
        window: &mut Window,
    ) -> Option<Pixels> {
        let width = self.layout.size(panel);
        let mut result = self.layout.is_open(panel).then_some(width);
        let mut finished = false;
        if let Some((_, transition)) = self.transitions.iter().find(|(other, _)| *other == panel) {
            let elapsed = transition.began.elapsed().as_secs_f32();
            let t: f32 = (elapsed / standard.as_secs_f32()).min(1.0);
            // Ease out: fast to leave, gentle to land.
            let eased = 1.0 - (1.0 - t).powi(3);
            let fraction = if transition.opening {
                eased
            } else {
                1.0 - eased
            };
            result = Some(width * fraction).filter(|w| *w > px(0.));
            if t >= 1.0 {
                finished = true;
                result = self.layout.is_open(panel).then_some(width);
            } else {
                window.request_animation_frame();
            }
        }
        if finished {
            self.transitions.retain(|(other, _)| *other != panel);
        }
        result
    }

    /// A divider was pressed.
    fn begin_resize(&mut self, panel: Panel, at: Pixels, cx: &mut Context<Self>) {
        self.resizing = Some(ColumnDrag {
            panel,
            start_x: at,
            start_layout: self.layout,
        });
        cx.notify();
    }

    /// The pointer moved while a divider is held.
    fn drag_to(&mut self, x: Pixels, window: &Window, cx: &mut Context<Self>) {
        let Some(drag) = self.resizing else {
            return;
        };
        let delta = x - drag.start_x;
        // The sidebar's divider is on its right, so the column grows with
        // `x`; the right panel's is on its left, so it shrinks.
        let wanted = match drag.panel {
            Panel::Sidebar => drag.start_layout.size(drag.panel) + delta,
            Panel::RightPanel | Panel::AgentPanel => drag.start_layout.size(drag.panel) - delta,
        };
        // The sidebar has a ceiling of its own; the right panel may take
        // whatever the centre's floor leaves it, because reading a diff
        // is what a wide right panel is for.
        if drag.panel == Panel::Sidebar {
            let before = self.layout;
            self.layout.resize_sidebar_from(
                drag.start_layout,
                wanted,
                window.viewport_size().width,
                SidebarResizeLimits {
                    centre_min: CENTRE_MIN,
                    sidebar_min: px(200.),
                    sidebar_max: px(480.),
                    right_min: px(280.),
                },
            );
            if self.layout != before {
                cx.notify();
            }
            return;
        }

        let (min, max) = match drag.panel {
            Panel::Sidebar => (px(200.), px(480.)),
            Panel::RightPanel => (px(280.), Pixels::MAX),
            Panel::AgentPanel => (px(320.), px(640.)),
        };
        // Neither column may squeeze the centre below its floor.
        let other = Panel::ALL
            .iter()
            .copied()
            .filter(|panel| *panel != drag.panel)
            .fold(px(0.), |total, panel| total + self.drawn_or_zero(panel));
        let room = window.viewport_size().width - other - CENTRE_MIN;
        let width = wanted.max(min).min(max).min(room.max(min));
        if width != self.layout.size(drag.panel) {
            self.layout.set_size(drag.panel, width);
            cx.notify();
        }
    }

    fn drawn_or_zero(&self, panel: Panel) -> Pixels {
        if self.layout.is_open(panel) {
            self.layout.size(panel)
        } else {
            px(0.)
        }
    }

    /// The divider was let go.
    fn end_resize(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            self.persist();
            cx.notify();
        }
    }

    /// The grab area on a column's edge.
    fn handle(&self, panel: Panel, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx);
        let active = self.resizing.is_some_and(|drag| drag.panel == panel);
        let line = tokens
            .colors()
            .accent
            .opacity(if active { 0.9 } else { 0.5 });
        let id = match panel {
            Panel::Sidebar => "handle-sidebar",
            Panel::RightPanel => "handle-right",
            Panel::AgentPanel => "handle-agent",
        };
        div()
            .id(id)
            .absolute()
            .top_0()
            .bottom_0()
            .w(HANDLE_WIDTH)
            .map(|this| match panel {
                Panel::Sidebar => this.right(-HANDLE_WIDTH / 2.),
                Panel::RightPanel | Panel::AgentPanel => this.left(-HANDLE_WIDTH / 2.),
            })
            .cursor_col_resize()
            .group("handle")
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(HANDLE_WIDTH / 2. - px(0.5))
                    .w(px(1.))
                    .when(active, |this| this.bg(line))
                    .group_hover("handle", |this| this.bg(line)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_resize(panel, event.position.x, cx);
                }),
            )
    }

    fn persist(&mut self) {
        self.layout.write_into(&mut self.settings);
        if let Err(error) = settings::save(&self.paths.app_settings(), &self.settings) {
            tracing::warn!(%error, "could not persist the window's settings");
        }
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

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    fn on_open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(window, cx);
    }

    /// Fetch again what is on screen: the sidebar's lists, the centre list,
    /// and the item being read.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| store.refresh_all(cx));
        self.list.update(cx, |list, cx| list.refresh(cx));
        self.browser.update(cx, |browser, cx| browser.refresh(cx));
        self.history.update(cx, |history, cx| history.refresh(cx));
        self.detail.update(cx, |detail, cx| detail.refresh(cx));
    }

    /// A small square control in a header strip.
    fn icon_button(
        &self,
        id: &'static str,
        icon: Icon,
        tip: String,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let tokens = Tokens::global(cx);
        let hover = tokens.colors().bg_raised;
        let radius = px(tokens.radius.row);
        div()
            .id(id)
            .p_1()
            .rounded(radius)
            .cursor_pointer()
            .hover(move |this| this.bg(hover))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(icon)
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    }

    /// A control that opens or closes a panel. The icon reports the *state*,
    /// so an open panel shows the "close" variant and reads without hovering.
    fn panel_toggle(
        &self,
        panel: Panel,
        open_icon: IconName,
        closed_icon: IconName,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open = self.layout.is_open(panel);
        let color = Tokens::global(cx).colors().text_secondary;
        let id = match panel {
            Panel::Sidebar => "toggle-sidebar",
            Panel::RightPanel => "toggle-right",
            Panel::AgentPanel => "toggle-agent",
        };
        self.icon_button(
            id,
            Icon::new(if open { open_icon } else { closed_icon })
                .size_4()
                .text_color(color),
            rust_i18n::t!(panel.label_key()).to_string(),
            cx,
            move |this, _, cx| this.toggle(panel, cx),
        )
    }

    /// Make a strip the window can be dragged by.
    ///
    /// A drag is a press that then moved: acting on the press alone would
    /// carry the window off whenever a control on the strip was clicked.
    fn draggable(&self, strip: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        strip
            .on_double_click(|_, window, _| window.titlebar_double_click())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = false),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.dragging = false))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.dragging {
                    this.dragging = false;
                    window.start_window_move();
                }
            }))
    }

    /// The window's own controls, across the top of the leading column: room
    /// for the traffic lights, then the sidebar toggle, and the palette at
    /// the far end.
    ///
    /// The palette belongs here rather than in the centre strip because what
    /// it opens onto is the whole window — the sections and the repositories
    /// this column lists, and the search — while the centre strip belongs to
    /// one repository.
    fn window_controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let strip = h_flex()
            .id("window-controls")
            .flex_shrink_0()
            .w_full()
            .h(HEADER_HEIGHT)
            .pl(TRAFFIC_LIGHT_INSET)
            .pr_2()
            .gap_3()
            .items_center()
            .child(self.panel_toggle(
                Panel::Sidebar,
                IconName::PanelLeftClose,
                IconName::PanelLeftOpen,
                cx,
            ))
            .child(div().flex_1())
            .children(self.palette_button("open-palette", cx));
        self.draggable(strip, cx)
    }

    /// The control that opens the palette, `None` when there is nothing to
    /// jump to yet.
    ///
    /// Drawn in whichever strip is the leading one, so exactly one is on
    /// screen: the sidebar's while it is open, the centre's while it is not.
    fn palette_button(&self, id: &'static str, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let secondary = Tokens::global(cx).colors().text_secondary;
        self.signed_in.then(|| {
            self.icon_button(
                id,
                Icon::new(IconName::Search).size_4().text_color(secondary),
                rust_i18n::t!("search.title").to_string(),
                cx,
                |this, window, cx| this.open_palette(window, cx),
            )
        })
    }

    /// A two-way toggle in the centre strip, drawn as a row of chips.
    fn chips<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: Vec<(T, String)>,
        current: T,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Self, T, &mut Context<Self>) + Clone + 'static,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        h_flex()
            .id(id)
            .gap_0p5()
            .p_0p5()
            .rounded(px(tokens.radius.row))
            .bg(tokens.colors().bg_surface)
            .children(
                options
                    .into_iter()
                    .enumerate()
                    .map(|(index, (value, label))| {
                        let selected = value == current;
                        let pick = pick.clone();
                        div()
                            .id((id, index))
                            .px_2()
                            .py_0p5()
                            .rounded(px(tokens.radius.row - 2.))
                            .cursor_pointer()
                            .text_size(px(11.5))
                            .when(selected, |this| {
                                this.bg(tokens.colors().row_active())
                                    .text_color(tokens.colors().text_primary)
                            })
                            .when(!selected, |this| {
                                this.text_color(tokens.colors().text_muted)
                            })
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| pick(this, value, cx)))
                    }),
            )
            .into_any_element()
    }

    /// The strip across the top of the centre column: what the list is, and
    /// the controls for it and for the panels either side.
    fn column_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let muted = tokens.colors().text_muted;
        let secondary = tokens.colors().text_secondary;
        let primary = tokens.colors().text_primary;
        let leading = !self.layout.is_open(Panel::Sidebar);
        let focus = self.focus(cx);
        let loading = self.list.read(cx).is_loading(cx) || self.browser.read(cx).is_loading(cx);
        let title: Option<SharedString> = if self.signed_in {
            focus.as_ref().map(|focus| focus.title().into())
        } else {
            Some(rust_i18n::t!("signin.title").to_string().into())
        };
        let subtitle: Option<SharedString> = focus
            .as_ref()
            .filter(|_| self.signed_in)
            .and_then(|focus| focus.subtitle())
            .map(Into::into);

        let sidebar_toggle = leading.then(|| {
            self.panel_toggle(
                Panel::Sidebar,
                IconName::PanelLeftClose,
                IconName::PanelLeftOpen,
                cx,
            )
        });
        let focus = focus.filter(|_| self.signed_in);
        let tab_chips = focus
            .as_ref()
            .and_then(|focus| focus.repo_tab())
            .map(|tab| {
                self.chips(
                    "tab",
                    RepoTab::ALL
                        .iter()
                        .map(|tab| (*tab, rust_i18n::t!(tab.label_key()).to_string()))
                        .collect(),
                    tab,
                    cx,
                    |this, tab, cx| {
                        if let Some(focus) = this.focus(cx).and_then(|focus| focus.with_tab(tab)) {
                            this.refocus(focus, cx);
                        }
                    },
                )
            });
        let status_chips = focus
            .as_ref()
            .and_then(|focus| match focus {
                Focus::Repo { status, .. } => Some(*status),
                _ => None,
            })
            .map(|status| {
                self.chips(
                    "status",
                    vec![
                        (StatusFilter::Open, rust_i18n::t!("list.open").to_string()),
                        (
                            StatusFilter::Closed,
                            rust_i18n::t!("list.closed").to_string(),
                        ),
                    ],
                    status,
                    cx,
                    |this, status, cx| {
                        if let Some(focus) =
                            this.focus(cx).and_then(|focus| focus.with_status(status))
                        {
                            this.refocus(focus, cx);
                        }
                    },
                )
            });
        // Only when this is the leading strip: the sidebar's own carries it
        // otherwise, and two would be one too many.
        let palette = leading
            .then(|| self.palette_button("open-palette-centre", cx))
            .flatten();
        let refresh = self.icon_button(
            "refresh",
            Icon::new(IconName::RotateCw)
                .size_4()
                .text_color(if loading {
                    tokens.colors().accent
                } else {
                    secondary
                }),
            rust_i18n::t!("list.refresh").to_string(),
            cx,
            |this, _, cx| this.refresh(cx),
        );
        let right_toggle = self.panel_toggle(
            Panel::RightPanel,
            IconName::PanelRightClose,
            IconName::PanelRightOpen,
            cx,
        );

        let strip = h_flex()
            .id("column-header")
            .flex_shrink_0()
            .w_full()
            .h(HEADER_HEIGHT)
            .pr_3()
            .gap_2()
            .items_center()
            .when(leading, |this| this.pl(TRAFFIC_LIGHT_INSET))
            .when(!leading, |this| this.pl_4())
            .children(sidebar_toggle)
            .child(
                h_flex()
                    .flex_1()
                    .gap_2()
                    .items_center()
                    .overflow_hidden()
                    .children(title.map(|title| {
                        div()
                            .text_size(px(13.))
                            .font_medium()
                            .text_color(primary)
                            .truncate()
                            .child(title)
                    }))
                    .children(subtitle.map(|subtitle| {
                        div()
                            .text_size(px(11.5))
                            .text_color(muted)
                            .truncate()
                            .child(subtitle)
                    })),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .children(tab_chips)
                    .children(status_chips)
                    .children(palette)
                    .child(refresh)
                    .child(right_toggle),
            );
        self.draggable(strip, cx)
    }

    /// The strip across the top of the right column: the way to the item on
    /// the web.
    fn right_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let url = self.detail.read(cx).html_url(cx);
        let open = url.map(|url| {
            self.icon_button(
                "open-on-github",
                Icon::new(IconName::ExternalLink)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
                rust_i18n::t!("detail.open_on_github").to_string(),
                cx,
                move |_, _, cx| cx.open_url(&url),
            )
        });
        let strip = h_flex()
            .id("right-header")
            .flex_shrink_0()
            .w_full()
            .h(HEADER_HEIGHT)
            .px_3()
            .gap_2()
            .items_center()
            .justify_end()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .children(open);
        self.draggable(strip, cx)
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let before = self.layout;
        self.layout.fit_to_viewport(
            window.viewport_size().width,
            CENTRE_MIN,
            px(200.),
            px(280.),
            px(320.),
        );
        if self.layout != before {
            self.persist();
        }
        if self.retheme {
            self.retheme = false;
            // Not here: `Window::refresh` does nothing while the window is
            // drawing, and this is the middle of a draw. Installing the
            // theme without it leaves the frame half switched — whatever
            // renders after this line takes the new colours and everything
            // already drawn, the window's own background included, keeps the
            // old ones until something else happens to invalidate it. A
            // deferred call runs once this frame is over, where the refresh
            // lands.
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |this, cx| this.apply_theme(window, cx));
            });
        }
        let tokens = Tokens::global(cx).clone();
        let standard = tokens.duration_ms.standard();
        let sidebar_width = self.drawn_width(Panel::Sidebar, standard, window);
        let right_width = self.drawn_width(Panel::RightPanel, standard, window);
        let agent_width = self.drawn_width(Panel::AgentPanel, standard, window);
        let sidebar_open = self.layout.is_open(Panel::Sidebar);

        // Built before the column chain: the headers bind listeners, and the
        // chain's own closures hold `self` while they run.
        let window_controls = sidebar_open.then(|| {
            div()
                .w_full()
                .bg(tokens.colors().bg_sidebar)
                .border_r_1()
                .border_color(tokens.colors().border_subtle)
                .child(self.window_controls(cx))
                .into_any_element()
        });
        let column_header = self.column_header(cx).into_any_element();
        let right_header = right_width.map(|_| self.right_header(cx).into_any_element());
        let centre: AnyElement = if !self.signed_in {
            self.sign_in.clone().into_any_element()
        } else if self.centre_is_browser(cx) {
            self.browser.clone().into_any_element()
        } else if self.centre_is_history(cx) {
            self.history.clone().into_any_element()
        } else {
            self.list.clone().into_any_element()
        };
        let sidebar_handle =
            sidebar_width.map(|_| self.handle(Panel::Sidebar, cx).into_any_element());
        let right_handle =
            right_width.map(|_| self.handle(Panel::RightPanel, cx).into_any_element());
        let agent_handle =
            agent_width.map(|_| self.handle(Panel::AgentPanel, cx).into_any_element());

        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_right_panel))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_open_palette))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_resize(cx)),
            )
            // While a divider is held the pointer is tracked at the window,
            // not on an element: an element only hears moves while it is
            // the one under the pointer, and a drag crosses text fields and
            // scrollbars that claim the pointer for themselves.
            .children(self.resizing.map(|_| {
                let this = cx.entity();
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        let on_move = this.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase.bubble() {
                                on_move.update(cx, |this, cx| {
                                    this.drag_to(event.position.x, window, cx)
                                });
                            }
                        });
                        let on_up = this.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase.bubble() {
                                on_up.update(cx, |this, cx| this.end_resize(cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_0()
            }))
            .size_full()
            // No background here: `Root` already paints the translucent
            // window and painting it again composites the alpha away.
            .text_color(tokens.colors().text_primary)
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .children(sidebar_width.map(|width| {
                        div()
                            .w(width)
                            .h_full()
                            .flex_shrink_0()
                            .relative()
                            .overflow_hidden()
                            .child(
                                v_flex()
                                    .w(self.layout.size(Panel::Sidebar))
                                    .h_full()
                                    .children(window_controls)
                                    // `flex_1` with a floor of zero: a view
                                    // that took the column's full height
                                    // under a 44 px strip ran 44 px past
                                    // the window, taking its footer with it.
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .w_full()
                                            .child(self.sidebar.clone()),
                                    ),
                            )
                            .children(sidebar_handle)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w(CENTRE_MIN)
                            .overflow_hidden()
                            .child(
                                v_flex()
                                    .size_full()
                                    .child(column_header)
                                    .child(div().flex_1().min_h_0().w_full().child(centre)),
                            ),
                    )
                    .children(right_width.map(|width| {
                        div()
                            .w(width)
                            .h_full()
                            .flex_shrink_0()
                            .relative()
                            .overflow_hidden()
                            .child(
                                v_flex()
                                    .w(self.layout.size(Panel::RightPanel))
                                    .h_full()
                                    .border_l_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .children(right_header)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .w_full()
                                            .child(self.detail.clone()),
                                    ),
                            )
                            .children(right_handle)
                    }))
                    .children(agent_width.map(|width| {
                        div()
                            .w(width)
                            .h_full()
                            .flex_shrink_0()
                            .relative()
                            .overflow_hidden()
                            .border_l_1()
                            .border_color(tokens.colors().border_subtle)
                            .child(
                                div()
                                    .w(self.layout.size(Panel::AgentPanel))
                                    .h_full()
                                    .child(self.agent.clone()),
                            )
                            .children(agent_handle)
                    })),
            )
            // A dialog is not drawn by the toolkit's `Root` on its own: the
            // window says where the layer goes, and it goes over everything.
            .children(gpui_component::Root::render_dialog_layer(window, cx))
    }
}
