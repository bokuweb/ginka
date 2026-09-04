//! The window shell: title bar plus the three resizable columns of
//! `docs/ui.md` §1.
//!
//! The centre column carries the transcript, the composer, the context bar and
//! the terminal dock. The transcript and composer are live: the composer starts
//! an agent in the selected workspace, or sends a follow-up to the session
//! already running there, and the transcript is folded from the daemon's event
//! stream. The terminal is still a placeholder (M3).

use crate::daemon::DaemonLink;
use crate::sidebar::{SessionSidebar, SidebarEvent};
use crate::surfaces::SurfacePanel;
use ginka_core::Paths;
use ginka_core::settings::{self, AppSettings};
use ginka_protocol::SessionId;
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::model::{AgentStatus, Checkpoint, SessionState, TranscriptEntry};
use ginka_ui::Tokens;
use ginka_ui::layout::{Layout, Panel};
use ginka_ui::transcript::{
    Activity, Applied, Block as TranscriptBlock, Reveal, Transcript, head_of,
};
use ginka_ui::workspace::SessionRow;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::text::TextView;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    Icon, IconName, StyledExt as _, TitleBar, h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    v_flex,
};
use std::sync::Arc;
use std::time::Duration;

actions!(
    shell,
    [
        ToggleSidebar,
        ToggleRightPanel,
        ToggleTerminalDock,
        TogglePalette
    ]
);

/// What the composer is offering a choice of.
///
/// One at a time: two open lists over a conversation is a menu, not a
/// composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picker {
    /// Which agent runs the next prompt.
    Agent,
    /// Which of that agent's models it runs on.
    Model,
    /// Which file the `@` being typed means.
    Mention,
    /// Which command the `/` being typed means.
    Command,
}

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
        // `docs/ui.md` §6: every action is reachable from here, so this is the
        // one chord that has to work wherever the focus happens to be.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", TogglePalette, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-k", TogglePalette, Some(CONTEXT)),
    ]);
}

/// How often worktrees and their git status are re-read.
///
/// The daemon pushes what it changes, so this is the backstop rather than the
/// mechanism: it catches what happens outside the daemon — a branch switched
/// in someone's terminal, a `git worktree add` — and it is what reconnects
/// after the daemon has been restarted.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// How long to wait before reaching for a daemon that was not there.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// The transcript's measure, in pixels: `docs/ui.md` §3.3's ~72 characters at
/// the 15px body size. Reading is what this column is for.
const TRANSCRIPT_MEASURE: f32 = 780.;

/// The line height the terminal's grid is drawn at, in pixels.
const TERMINAL_LINE_HEIGHT: f32 = 17.;

/// How wide a terminal is told it is.
///
/// Fixed rather than measured: the dock's width changes with every window
/// resize, and a shell told a new width on every frame spends its time
/// re-wrapping instead of working. Eighty is what everything assumes anyway.
const TERMINAL_COLUMNS: u16 = 100;

/// How far from the foot still counts as being at it. About a line of text:
/// enough that an answer growing between one frame and the next does not read
/// as the reader scrolling away.
const NEARLY_THE_FOOT: f32 = 24.;

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
    /// Everything the app knows is behind this.
    link: Arc<crate::daemon::DaemonLink>,
    /// The selected session's transcript, folded from the daemon's events.
    transcript: Transcript,
    /// Which session the transcript belongs to, so switching rows replaces it
    /// rather than appending one conversation to another.
    transcript_of: Option<SessionId>,
    /// What each agent CLI on this machine says about itself, so the composer
    /// can say which agent it would start and whether it will work.
    agents: Vec<AgentStatus>,
    /// How much of the answer being written is on screen.
    reveal: Reveal,
    /// What the daemon last said the shown session was doing.
    ///
    /// Followed from its pushes rather than from the sidebar's rows, which are
    /// only re-read on a tick: an agent that started fifteen seconds before
    /// the window admits it has started is an agent the user assumes is broken.
    session_state: Option<SessionState>,
    /// Set the moment a prompt is handed over, so the window says it is
    /// working before the daemon has had a chance to answer.
    submitted: bool,
    /// Whether the composer has the keyboard, so the card can show it.
    composer_focused: bool,
    /// Which picker is open above the composer, if any.
    picker: Option<Picker>,
    /// The files offered for the mention being typed, if one is.
    mentions: Vec<ginka_protocol::model::FileEntry>,
    /// The commands offered for the `/` being typed, if one is.
    commands: Vec<ginka_protocol::model::SlashCommand>,
    /// The agent the user chose, which beats whatever would have been picked
    /// for them. `None` until they choose one.
    chosen_agent: Option<String>,
    /// The model the user chose for that agent.
    chosen_model: Option<String>,
    /// Set when the next prompt should open a new conversation rather than
    /// continue the one on screen.
    start_fresh: bool,
    /// The states this workspace can be put back to, one per turn.
    checkpoints: Vec<Checkpoint>,
    /// The turn whose rewind has been offered and is waiting to be confirmed.
    ///
    /// Two steps because it is destructive: work written since is removed, and
    /// a single click that quietly rewrites the worktree is not something to
    /// discover by accident.
    rewinding: Option<u32>,
    /// Where keystrokes go when the terminal has the keyboard.
    terminal_focus: FocusHandle,
    /// The command palette, while it is open: what is typed into it, the
    /// entries that match, and which one Return would run.
    palette: Option<Palette>,
    /// The shells in the dock, and what each has printed.
    ///
    /// The daemon owns the ptys; these are the screens they are drawn on,
    /// which is why a window that closes and reopens finds the build still
    /// running and picks the tab back up.
    terminals: ginka_ui::terminal::TerminalTabs,
    /// The transcript's scroll position, so the answer can be followed.
    transcript_scroll: ScrollHandle,
    /// Whether the transcript is still following the answer. Dropped by the
    /// reader scrolling away, restored by them coming back to the foot.
    transcript_follows: bool,
    composer: Entity<TextareaState>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rows: Vec<SessionRow> = Vec::new();
        let link = crate::daemon::DaemonLink::new(&paths);
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
        let surfaces = cx.new(|cx| SurfacePanel::new(window, cx));

        let committing = cx.subscribe(&surfaces, |this, _, event, cx| match event {
            crate::surfaces::SurfaceEvent::Commit {
                message,
                only_staged,
            } => this.commit(message.clone(), *only_staged, cx),
            crate::surfaces::SurfaceEvent::Comment { path, line, text } => {
                this.leave_comment(path.clone(), *line, text.clone(), cx)
            }
            crate::surfaces::SurfaceEvent::SendReview => this.send_review(cx),
            crate::surfaces::SurfaceEvent::Stage { path, staged } => {
                this.stage(path.clone(), *staged, cx)
            }
            crate::surfaces::SurfaceEvent::Revert { path } => this.revert(path.clone(), cx),
            crate::surfaces::SurfaceEvent::FindFiles(query) => this.find_files(query.clone(), cx),
            crate::surfaces::SurfaceEvent::OpenFile(path) => this.open_file(path.clone(), cx),
        });

        let selection =
            cx.subscribe_in(
                &sidebar,
                window,
                |this, sidebar, event, window, cx| match event {
                    SidebarEvent::Selected => {
                        this.session = sidebar.read(cx).selected_row().cloned();
                        // A different workspace is a different conversation.
                        this.transcript = Transcript::new();
                        this.transcript_of = None;
                        this.session_state = None;
                        this.submitted = false;
                        this.transcript_follows = true;
                        this.picker = None;
                        this.chosen_agent = None;
                        this.chosen_model = None;
                        this.start_fresh = false;
                        this.checkpoints = Vec::new();
                        this.rewinding = None;
                        // The shells belong to the workspace, not to the
                        // window: a different workspace is a different strip.
                        this.terminals = ginka_ui::terminal::TerminalTabs::new();
                        if this.layout.is_open(Panel::TerminalDock) {
                            this.adopt_terminals(cx);
                        }
                        this.mentions.clear();
                        // Whatever was half-written here when it was last left.
                        this.composer
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.load_draft(window, cx);
                        cx.notify();
                    }
                },
            );

        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("composer.placeholder").to_string())
                // Grows with the prompt up to a point, then scrolls: a long
                // paste must not swallow the transcript.
                .auto_grow(1, 8)
                // Enter sends, shift-Enter is a newline -- the convention
                // every chat surface the user already has works this way.
                .submit_on_enter(true)
        });
        let submitted = cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => {
                    // A mention being chosen is not a message being sent.
                    // Something being chosen is not a message being sent.
                    match this.picker {
                        Some(Picker::Mention) => this.take_first_mention(window, cx),
                        Some(Picker::Command) => this.take_first_command(window, cx),
                        _ => this.submit(window, cx),
                    }
                }
                InputEvent::Change => this.composer_changed(window, cx),
                InputEvent::Focus => {
                    this.composer_focused = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.composer_focused = false;
                    cx.notify();
                }
                _ => {}
            },
        );

        // Worktrees change outside the app -- an agent commits, the user
        // switches a branch in a terminal, someone runs `git worktree add`. A
        // tick is how those reach the window without the user reopening it.
        // The daemon pushes those same changes as events; following the push
        // instead of polling is M2's remaining piece.
        cx.spawn({
            let link = link.clone();
            async move |this, cx| {
                // The first pass is immediate: the window is already open and
                // empty, and waiting a whole tick to fill it reads as a stall.
                let mut delay = std::time::Duration::ZERO;
                loop {
                    cx.background_executor().timer(delay).await;
                    delay = REFRESH_INTERVAL;
                    let Ok(session) = pull_rows(&this, &link, cx).await else {
                        // The window is gone; stop ticking.
                        break;
                    };
                    if let Some(session) = session
                        && pull_transcript(&this, &link, session, cx).await.is_err()
                    {
                        break;
                    }
                }
            }
        })
        .detach();

        // The tick is the backstop. What makes the window feel live is the
        // daemon's own stream: an agent's text arrives as it is produced,
        // rather than up to a tick later.
        cx.spawn({
            let link = link.clone();
            async move |this, cx| {
                loop {
                    let waiting = link.clone();
                    let event = cx
                        .background_spawn(async move { waiting.next_event().await })
                        .await;
                    let Some(event) = event else {
                        // No daemon, or it stopped. Wait rather than spinning
                        // on a socket that is not there; the tick reconnects.
                        cx.background_executor().timer(RECONNECT_DELAY).await;
                        continue;
                    };

                    let followed = match event.payload {
                        // A session's own output: fold in the tail if it is the
                        // one on screen, and otherwise leave it to the tick —
                        // another workspace's typing is not this column's news.
                        // The push carries the whole entry, so it goes
                        // straight in: that is what makes a prompt and the
                        // answer to it appear as they happen rather than a
                        // round trip later. A push this window cannot place —
                        // a gap, or a session it is not showing — falls back
                        // to a read.
                        DaemonEvent::SessionEvent { session, entry } => {
                            let folded =
                                this.update(cx, |this, cx| this.fold_pushed(&session, entry, cx));
                            match folded {
                                Err(_) => Err(()),
                                Ok(true) => Ok(()),
                                Ok(false) => {
                                    let showing = this
                                        .update(cx, |this, _| {
                                            this.session
                                                .as_ref()
                                                .and_then(|row| row.session.clone())
                                        })
                                        .ok()
                                        .flatten();
                                    match showing {
                                        Some(showing) if showing == session => {
                                            pull_transcript(&this, &link, session, cx).await
                                        }
                                        _ => Ok(()),
                                    }
                                }
                            }
                        }
                        // What the agent is doing, as soon as it starts doing
                        // it. The sidebar's own rows follow on the next tick.
                        DaemonEvent::SessionStateChanged { session, state } => this
                            .update(cx, |this, cx| {
                                if this.session.as_ref().and_then(|row| row.session.as_ref())
                                    == Some(&session)
                                {
                                    this.session_state = Some(state);
                                    this.submitted = false;
                                    cx.notify();
                                }
                            })
                            .map_err(|_| ()),
                        // Anything that changes what the sidebar says.
                        DaemonEvent::ProjectsChanged
                        | DaemonEvent::WorkspacesChanged { .. }
                        | DaemonEvent::WorkspaceStatusChanged { .. }
                        | DaemonEvent::SessionStarted { .. } => {
                            pull_rows(&this, &link, cx).await.map(|_| ())
                        }
                        // The shell printed something. Straight onto its
                        // screen: a terminal that lagged behind what was typed
                        // into it would be unusable for the thing terminals
                        // are for.
                        DaemonEvent::TerminalOutput { terminal, data } => this
                            .update(cx, |this, cx| {
                                if this.terminals.feed(&terminal, &data) {
                                    cx.notify();
                                }
                            })
                            .map_err(|_| ()),
                        DaemonEvent::TerminalClosed { terminal } => this
                            .update(cx, |this, cx| {
                                this.terminals.close(&terminal);
                                cx.notify();
                            })
                            .map_err(|_| ()),
                        DaemonEvent::Shutdown => {
                            cx.background_executor().timer(RECONNECT_DELAY).await;
                            Ok(())
                        }
                    };
                    if followed.is_err() {
                        break;
                    }
                }
            }
        })
        .detach();
        // A window that opens with the dock already open has shells waiting
        // for it: the daemon kept them running, and finding them is what makes
        // the process split visible rather than theoretical.
        cx.defer_in(window, |this, _, cx| {
            if this.layout.is_open(Panel::TerminalDock) {
                this.adopt_terminals(cx);
            }
        });
        Self {
            layout: Layout::from_settings(&settings),
            link,
            transcript: Transcript::new(),
            transcript_of: None,
            agents: Vec::new(),
            reveal: Reveal::new(),
            palette: None,
            terminal_focus: cx.focus_handle(),
            terminals: ginka_ui::terminal::TerminalTabs::new(),
            session_state: None,
            submitted: false,
            composer_focused: false,
            picker: None,
            mentions: Vec::new(),
            commands: Vec::new(),
            chosen_agent: None,
            chosen_model: None,
            start_fresh: false,
            checkpoints: Vec::new(),
            rewinding: None,
            transcript_scroll: ScrollHandle::new(),
            transcript_follows: true,
            composer,
            paths,
            settings,
            session,
            sidebar,
            surfaces,
            _subscriptions: vec![appearance, selection, submitted, committing],
        }
    }

    /// Where the folded transcript for `session` has reached.
    ///
    /// A different session reads as position zero, because its transcript has
    /// not been read at all yet.
    fn transcript_cursor(&self, session: &SessionId) -> u64 {
        match &self.transcript_of {
            Some(current) if current == session => self.transcript.cursor(),
            _ => 0,
        }
    }

    /// Fold a page of transcript entries in.
    ///
    /// A page that reveals a hole — the daemon published something this window
    /// never saw — drops the folded transcript and starts again from the
    /// beginning of the session, because a transcript with a gap in it reads
    /// as a conversation that did not happen.
    fn fold(&mut self, session: &SessionId, entries: &[TranscriptEntry], cx: &mut Context<Self>) {
        // A transcript read from storage is history, and history is shown
        // whole: watching a conversation you have already had being typed out
        // is a pointless wait.
        let opening = self.transcript_of.as_ref() != Some(session);
        if opening {
            self.transcript = Transcript::new();
            self.transcript_of = Some(session.clone());
            self.reveal.reset();
        }
        if entries.is_empty() {
            return;
        }
        if let Applied::Gap { expected } = self.transcript.extend(entries) {
            tracing::warn!(%session, expected, "transcript gap; re-reading the session");
            self.transcript = Transcript::new();
            self.reveal.reset();
        }
        if opening {
            self.reveal.show_all(
                self.transcript
                    .tail()
                    .map(|tail| tail.chars().count())
                    .unwrap_or(0),
            );
        }
        cx.notify();
    }

    /// Fold one pushed event straight in.
    ///
    /// The push carries the event and its position, so there is nothing to go
    /// back and fetch: this is what makes an agent's text appear as it is
    /// produced rather than a round trip later. A push from beyond the next
    /// position means something was missed, and the caller re-reads instead.
    fn fold_pushed(
        &mut self,
        session: &SessionId,
        entry: TranscriptEntry,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.transcript_of.as_ref() != Some(session) {
            return false;
        }
        match self.transcript.apply(&entry) {
            Applied::Added => {
                cx.notify();
                true
            }
            Applied::AlreadySeen => true,
            Applied::Gap { .. } => false,
        }
    }

    /// Walk the answer onto the screen, one drawn frame at a time.
    ///
    /// Ported from bokuweb/pedro: this runs from `render` rather than from a
    /// timer, and that is the point. At a fixed beat each step has to carry
    /// whatever the agent produced in that beat, which is a chunk however
    /// smoothly the rate was eased into it. Asking for the next frame is what
    /// keeps it going; with nothing left to write nothing is asked for, and
    /// the window goes back to sleep.
    fn write_a_little_more(&mut self, window: &mut Window) {
        self.follow_the_answer();

        let arrived = self
            .transcript
            .tail()
            .map(|tail| tail.chars().count())
            .unwrap_or(0);
        if self.reveal.advance(arrived) {
            window.request_animation_frame();
        }
    }

    /// Keep the foot of the conversation in view while it is being written.
    fn follow_the_answer(&mut self) {
        let (pull, follows) = ginka_ui::transcript::following(
            self.is_working(),
            self.transcript_follows,
            self.transcript_at_foot(),
        );
        self.transcript_follows = follows;
        if pull {
            self.transcript_scroll.scroll_to_bottom();
        }
    }

    /// Whether the transcript is scrolled to its foot, near enough.
    ///
    /// Near enough because the foot moves as the answer grows: by the time a
    /// frame is drawn the text is a line longer than when the offset was set.
    fn transcript_at_foot(&self) -> bool {
        let offset = f32::from(self.transcript_scroll.offset().y);
        let furthest = f32::from(self.transcript_scroll.max_offset().y);
        // Scrolling down counts down from zero, so the foot is the most
        // negative the offset gets.
        furthest + offset <= NEARLY_THE_FOOT
    }

    /// The reader has taken the transcript somewhere themselves.
    fn transcript_scrolled(&mut self) {
        self.transcript_follows = false;
    }

    /// Put the workspace back to a checkpoint, and read the transcript again.
    ///
    /// The transcript is not rewound with it: what the agent said still
    /// happened, and a conversation that edits itself to match the files is a
    /// worse record than one that shows the work being undone.
    fn rewind(&mut self, checkpoint: ginka_protocol::CheckpointId, cx: &mut Context<Self>) {
        self.rewinding = None;
        cx.notify();
        let link = self.link.clone();
        cx.spawn(async move |_, cx| {
            cx.background_spawn(async move { link.restore(&checkpoint).await })
                .await;
        })
        .detach();
    }

    /// Commit the workspace's work, and say so if git would not.
    fn commit(&mut self, message: String, only_staged: bool, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_spawn(
                    async move { link.commit(&workspace, message, !only_staged).await },
                )
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_commit_result(outcome.err(), cx)
            });
        })
        .detach();
    }

    /// Put a file into the next commit, or take it back out.
    fn stage(&mut self, path: String, staged: bool, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move { link.stage(&workspace, &path, staged).await })
                .await;
            // The staged set and the diff both moved; the refresh reads both.
            this.update(cx, |this, cx| this.refresh_changes(cx)).ok();
        })
        .detach();
    }

    /// Throw away a file's uncommitted work.
    fn revert(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move { link.revert(&workspace, &path).await })
                .await;
            this.update(cx, |this, cx| this.refresh_changes(cx)).ok();
        })
        .detach();
    }

    /// Re-read the diff, the staged set and the comments for the workspace on
    /// screen, without waiting for the next tick.
    fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let (changes, staged, comments) = cx
                .background_spawn(async move {
                    let changes = link
                        .changes(&workspace, ginka_protocol::ChangeSource::Uncommitted)
                        .await;
                    let staged = link.staged_paths(&workspace).await;
                    let comments = link.comments(&workspace).await;
                    (changes, staged, comments)
                })
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_changes(changes, cx);
                surfaces.set_staged(staged, cx);
                surfaces.set_comments(comments, cx);
            });
        })
        .detach();
    }

    /// Find the workspace's files that match what was typed into the finder.
    fn find_files(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let (found, matched) = cx
                .background_spawn(async move {
                    // Both at once: a reader who knows what the code says and
                    // not what it is called is asking the same question.
                    let found = link.files(&workspace, &query).await;
                    let matched = link.search_content(&workspace, &query).await;
                    (found, matched)
                })
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_files(found, cx);
                surfaces.set_matches(matched, cx);
            });
        })
        .detach();
    }

    /// Read a file and show it in the files surface.
    fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let file = cx
                .background_spawn(async move { link.read_file(&workspace, &path).await })
                .await;
            surfaces.update(cx, |surfaces, cx| surfaces.set_file(file, cx));
        })
        .detach();
    }

    /// Start a shell in the workspace on screen.
    ///
    /// Sized for the dock as it is now, and focused, because someone who
    /// opened a terminal means to type in it.
    fn open_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let (rows, cols) = self.dock_size();
        let link = self.link.clone();
        self.terminal_focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move { link.open_terminal(&workspace, rows, cols).await })
                .await;
            if let Some(terminal) = opened {
                this.update(cx, |this, cx| {
                    let title = this.terminals.tabs().len() + 1;
                    this.terminals
                        .open(terminal, format!("shell {title}"), rows, cols);
                    // The daemon is the one that names them, and it numbers
                    // them per workspace: adopting straight afterwards is how
                    // two windows agree on what a tab is called.
                    this.adopt_terminals(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Stop one of the dock's shells.
    fn close_terminal(&mut self, terminal: ginka_protocol::TerminalId, cx: &mut Context<Self>) {
        self.terminals.close(&terminal);
        cx.notify();
        let link = self.link.clone();
        cx.background_spawn(async move { link.close_terminal(&terminal).await })
            .detach();
    }

    /// Bring one of the dock's shells to the front.
    fn show_terminal(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.terminals.focus(index);
        self.terminal_focus.focus(window, cx);
        cx.notify();
    }

    /// Find the shells the daemon kept running in this workspace, and replay
    /// what they printed while this window was not looking.
    fn adopt_terminals(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let (rows, cols) = self.dock_size();
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let running = cx
                .background_spawn(async move { link.terminals(&workspace).await })
                .await;
            let fresh = this
                .update(cx, |this, cx| {
                    cx.notify();
                    this.terminals.adopt(&running, rows, cols)
                })
                .ok()
                .unwrap_or_default();
            for terminal in fresh {
                let Ok(link) = this.update(cx, |this, _| this.link.clone()) else {
                    return;
                };
                let asked = terminal.clone();
                let history = cx
                    .background_spawn(async move { link.terminal_history(&asked).await })
                    .await;
                if let Some(history) = history {
                    this.update(cx, |this, cx| {
                        this.terminals.feed(&terminal, &history);
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// How many rows and columns the dock has room for.
    ///
    /// From the dock's height and the window's width at the mono metrics the
    /// screen is drawn with. Approximate on purpose: the shell only needs to
    /// know roughly how much room it has, and being a column out is better
    /// than measuring the grid every frame.
    fn dock_size(&self) -> (u16, u16) {
        let height = f32::from(self.layout.size(Panel::TerminalDock));
        let rows = ((height - 40.) / TERMINAL_LINE_HEIGHT).max(4.) as u16;
        (rows, TERMINAL_COLUMNS)
    }

    /// Send a keystroke to the shell.
    ///
    /// Translated here rather than passed as a key name, because a pty takes
    /// bytes: what a terminal *is* is a program reading the bytes a keyboard
    /// produced.
    fn type_into_terminal(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.active_id() else {
            return;
        };
        let Some(data) = keystroke_bytes(&event.keystroke) else {
            return;
        };
        let link = self.link.clone();
        cx.background_spawn(async move { link.write_terminal(&terminal, data).await })
            .detach();
    }

    /// Leave a comment on a line of the diff.
    fn leave_comment(
        &mut self,
        path: String,
        line: Option<u32>,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let comments = cx
                .background_spawn(async move {
                    link.add_comment(&workspace, &path, line, text).await;
                    link.comments(&workspace).await
                })
                .await;
            surfaces.update(cx, |surfaces, cx| surfaces.set_comments(comments, cx));
        })
        .detach();
    }

    /// Send the waiting comments to the agent as one message.
    fn send_review(&mut self, cx: &mut Context<Self>) {
        let (Some(workspace), Some(session)) = (
            self.session.as_ref().map(|row| row.workspace.clone()),
            self.session.as_ref().and_then(|row| row.session.clone()),
        ) else {
            return;
        };
        // The agent is about to be given work, so the window should say so.
        self.submitted = true;
        cx.notify();

        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let comments = cx
                .background_spawn(async move {
                    link.send_review(&workspace, &session).await;
                    link.comments(&workspace).await
                })
                .await;
            surfaces.update(cx, |surfaces, cx| surfaces.set_comments(comments, cx));
        })
        .detach();
    }

    /// Stop the agent working in the workspace on screen.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |_, cx| {
            cx.background_spawn(async move { link.cancel_session(&session).await })
                .await;
        })
        .detach();
    }

    /// Whether the workspace on screen has an agent working in it.
    ///
    /// Three sources, in order of how fresh they are: what this window just
    /// sent, what the daemon last pushed, and what the sidebar's last refresh
    /// said. The first is what makes the answer feel immediate.
    fn is_working(&self) -> bool {
        if self.submitted {
            return true;
        }
        match self.session_state {
            Some(state) => matches!(state, SessionState::Starting | SessionState::Running),
            None => self
                .session
                .as_ref()
                .is_some_and(|row| row.state == ginka_ui::workspace::AgentState::Working),
        }
    }

    /// Send what is in the composer.
    ///
    /// With a session already in the workspace this is a follow-up, queued by
    /// the daemon if the agent is mid-turn. Without one it starts an agent.
    /// Either way the box is cleared straight away: the prompt is the daemon's
    /// now, and leaving it behind invites sending it twice.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(row) = self.session.clone() else {
            tracing::warn!("nothing to send to: no workspace is selected");
            return;
        };
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        // The draft belonged to the prompt that has just been sent.
        {
            let link = self.link.clone();
            let workspace = row.workspace.clone();
            cx.background_spawn(async move { link.save_draft(&workspace, String::new()).await })
                .detach();
        }
        // Before the daemon has answered: the user pressed enter, and the
        // window saying nothing for a round trip reads as a window that
        // dropped it.
        self.submitted = true;
        self.transcript_follows = true;
        cx.notify();

        let link = self.link.clone();
        let agent = row.agent_to_start(&self.agents, self.chosen_agent.as_deref());
        let model = self.chosen_model.clone();
        let fresh = self.start_fresh;
        self.start_fresh = false;
        cx.spawn(async move |this, cx| {
            match row.session.clone().filter(|_| !fresh) {
                Some(session) => {
                    let sending = link.clone();
                    let text = text.clone();
                    cx.background_spawn(async move { sending.send_message(&session, text).await })
                        .await;
                }
                None => {
                    let workspace = row.workspace.clone();
                    let started = cx
                        .background_spawn(async move {
                            link.start_session(&workspace, &agent, text, model).await
                        })
                        .await;
                    // Adopt the new session immediately rather than waiting for
                    // the next tick: the user has just pressed enter and wants
                    // to see their prompt.
                    match started {
                        Some(session) => {
                            this.update(cx, |this, cx| {
                                if let Some(row) = this.session.as_mut() {
                                    row.session = Some(session.id.clone());
                                }
                                this.transcript = Transcript::new();
                                this.transcript_of = Some(session.id);
                                this.session_state = Some(session.state);
                                cx.notify();
                            })
                            .ok();
                        }
                        // It never started; stop claiming it is working.
                        None => {
                            this.update(cx, |this, cx| {
                                this.submitted = false;
                                cx.notify();
                            })
                            .ok();
                        }
                    }
                }
            }
        })
        .detach();
    }

    fn toggle(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.layout.toggle(panel);
        self.persist();
        // A dock that has just opened is one that has to find the shells the
        // daemon kept running while it was closed.
        if panel == Panel::TerminalDock && self.layout.is_open(panel) {
            self.adopt_terminals(cx);
        }
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
    /// Tell the shell how big its window is now.
    ///
    /// The pty has to be told separately from the screen: the shell learns its
    /// size from the pty, and a full-screen program laying out for the wrong
    /// one is the visible symptom of forgetting.
    fn resize_terminal(&mut self, cx: &mut Context<Self>) {
        let (rows, cols) = self.dock_size();
        // Every shell, not only the one in front: a tab brought forward after
        // the dock was resized would otherwise be laid out for the old size.
        let stale: Vec<ginka_protocol::TerminalId> = self
            .terminals
            .tabs()
            .iter()
            .filter(|tab| tab.screen.rows() != rows || tab.screen.cols() != cols)
            .map(|tab| tab.id.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        for tab in self.terminals.tabs_mut() {
            tab.screen.resize(rows, cols);
        }
        let link = self.link.clone();
        cx.background_spawn(async move {
            for terminal in stale {
                link.resize_terminal(&terminal, rows, cols).await;
            }
        })
        .detach();
        cx.notify();
    }

    fn record_resize(
        &mut self,
        slots: Vec<Option<Panel>>,
        state: &Entity<ResizableState>,
        cx: &mut Context<Self>,
    ) {
        let sizes = state.read(cx).sizes().clone();
        self.layout.record_sizes(&slots, &sizes);
        self.persist();
        // A dock that changed height is a shell with a different number of
        // lines to draw into.
        self.resize_terminal(cx);
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

    /// Open the palette, or close it if it is already open.
    fn on_toggle_palette(
        &mut self,
        _: &TogglePalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.take().is_some() {
            cx.notify();
            return;
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("palette.placeholder").to_string())
        });
        query.read(cx).focus_handle(cx).focus(window, cx);
        cx.subscribe(&query, |this, query, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let typed = query.read(cx).value().to_string();
                if let Some(palette) = this.palette.as_mut() {
                    palette.typed = typed;
                    // The list moved under the cursor, so the cursor goes back
                    // to the top: Return must never run something the reader
                    // has not looked at.
                    palette.chosen = 0;
                }
                cx.notify();
            }
        })
        .detach();
        self.palette = Some(Palette {
            query,
            typed: String::new(),
            chosen: 0,
        });
        cx.notify();
    }

    /// The entries the palette is offering, in the order it offers them.
    fn palette_entries(&self, cx: &App) -> Vec<ginka_ui::palette::Entry> {
        let Some(palette) = self.palette.as_ref() else {
            return Vec::new();
        };
        let rows = self.sidebar.read(cx).rows().to_vec();
        ginka_ui::palette::filter(
            ginka_ui::palette::entries(&self.layout, &rows),
            &palette.typed,
        )
    }

    /// Move the cursor through the palette without leaving the keyboard.
    fn palette_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.palette_entries(cx);
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        match event.keystroke.key.as_str() {
            "escape" => {
                self.palette = None;
                cx.notify();
            }
            "down" => {
                palette.chosen = (palette.chosen + 1).min(found.len().saturating_sub(1));
                cx.notify();
            }
            "up" => {
                palette.chosen = palette.chosen.saturating_sub(1);
                cx.notify();
            }
            "enter" => {
                if let Some(entry) = found.get(palette.chosen) {
                    let command = entry.command.clone();
                    self.palette = None;
                    self.run_command(command, window, cx);
                }
            }
            _ => {}
        }
    }

    /// Do what a palette entry says.
    fn run_command(
        &mut self,
        command: ginka_ui::palette::Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ginka_ui::palette::Command;
        match command {
            Command::TogglePanel(panel) => self.toggle(panel, cx),
            Command::ShowSurface(surface) => {
                // A surface nobody can see is not shown: opening the panel is
                // part of showing what is in it.
                if !self.layout.is_open(Panel::RightPanel) {
                    self.toggle(Panel::RightPanel, cx);
                }
                self.surfaces
                    .update(cx, |surfaces, cx| surfaces.show(surface, cx));
            }
            Command::NewTerminal => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.open_terminal(window, cx);
            }
            Command::Switch(workspace) => {
                self.sidebar
                    .update(cx, |sidebar, cx| sidebar.select_workspace(&workspace, cx));
            }
        }
        cx.notify();
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

    /// The conversation, folded from the daemon's event stream.
    fn transcript(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("transcript")
            .flex_1()
            .px_8()
            .py_6()
            .overflow_y_scroll()
            .track_scroll(&self.transcript_scroll)
            // The gesture, not the resulting offset: an answer that grows
            // moves the foot away from the reader too.
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.transcript_scrolled()))
            .child(
                // One column for the whole conversation, centred, at the
                // measure from docs/ui.md §3.3: long-form text stays readable
                // because the column stops growing, not because the window
                // does. Centred rather than pinned left because the composer
                // sits under it at the same width, and a conversation stranded
                // against one edge of a wide window reads as a mistake.
                v_flex()
                    .w_full()
                    .max_w(px(TRANSCRIPT_MEASURE))
                    .mx_auto()
                    .gap_5()
                    .when(self.transcript.is_empty(), |this| {
                        this.child(self.transcript_empty_state(cx))
                    })
                    .children({
                        let last = self.transcript.blocks().len().saturating_sub(1);
                        self.transcript
                            .blocks()
                            .iter()
                            .enumerate()
                            .map(|(index, block)| match block {
                                // Only the tail is still being written; every
                                // block before it is finished text.
                                TranscriptBlock::Assistant { text } if index == last => self.block(
                                    index,
                                    &TranscriptBlock::Assistant {
                                        text: self.reveal.shown(text).to_string(),
                                    },
                                    cx,
                                ),
                                block => self.block(index, block, cx),
                            })
                            .collect::<Vec<_>>()
                    })
                    .children(self.activity_line(cx)),
            )
    }

    /// The line that says the agent is still there.
    ///
    /// An agent between tokens looks exactly like one that has died, and the
    /// first token can take a few seconds. Saying which is which is the
    /// difference between waiting and wondering. It is not shown while text is
    /// arriving: the words are the indicator then.
    fn activity_line(&self, cx: &App) -> Option<AnyElement> {
        if !self.is_working() {
            return None;
        }
        let tokens = Tokens::global(cx);
        let label = match self.transcript.activity() {
            Activity::Writing => return None,
            Activity::Thinking => rust_i18n::t!("transcript.thinking").to_string(),
            Activity::Running { tool } if tool.is_empty() => {
                rust_i18n::t!("transcript.working").to_string()
            }
            Activity::Running { tool } => {
                rust_i18n::t!("transcript.running", tool = tool).to_string()
            }
        };

        Some(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(tokens.colors().status_working)
                        // Breathing rather than blinking: the point is that
                        // something is alive, not that something is wrong.
                        .with_animation(
                            "thinking-pulse",
                            Animation::new(Duration::from_millis(1_400))
                                .repeat_synced()
                                .with_easing(gpui::ease_in_out),
                            |this, delta| {
                                this.opacity(0.35 + 0.65 * (1. - (delta - 0.5).abs() * 2.))
                            },
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(label),
                )
                .into_any_element(),
        )
    }

    /// What the centre column says before there is a conversation in it.
    fn transcript_empty_state(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let body = match &self.session {
            Some(session) => rust_i18n::t!(
                "transcript.empty.ready",
                title = session.title,
                branch = session.branch
            )
            .to_string(),
            None => rust_i18n::t!("transcript.empty.no_project").to_string(),
        };
        div()
            .w_full()
            .text_size(px(15.))
            .line_height(px(25.))
            .text_color(tokens.colors().text_muted)
            .child(body)
    }

    /// One folded block of the transcript.
    ///
    /// Each kind is drawn differently on purpose: the reader has to be able to
    /// tell what the agent said from what it was thinking, and both from what
    /// it did.
    fn block(&self, index: usize, block: &TranscriptBlock, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let prose = |text: &str, color: Hsla| {
            div()
                .w_full()
                .text_size(px(15.))
                .line_height(px(25.))
                .text_color(color)
                .child(text.to_string())
        };

        match block {
            TranscriptBlock::User { text } => h_flex()
                .w_full()
                .justify_end()
                .child(
                    div()
                        // Never the full measure: a message that fills the
                        // column is indistinguishable from the agent's reply.
                        .max_w(px(TRANSCRIPT_MEASURE * 0.8))
                        .px(px(13.))
                        .py(px(9.))
                        .rounded(px(tokens.radius.panel + 2.))
                        // Tinted rather than another grey box: the reader's
                        // own words are the one thing on screen that is not
                        // the agent's.
                        .bg(tokens.colors().row_active())
                        .text_size(px(15.))
                        .line_height(px(25.))
                        .text_color(tokens.colors().text_primary)
                        .child(text.clone()),
                )
                .into_any_element(),
            TranscriptBlock::Assistant { text } => {
                // Agents answer in markdown — headings, lists, fenced code —
                // and reading it raw is reading the punctuation instead of the
                // answer. Only the finished blocks are formatted: markdown of
                // half a document re-flows the text under the reader on every
                // frame, so the line still being written is drawn as the plain
                // text it is until its block is done.
                let (formatted, writing) = ginka_ui::transcript::settled(text);
                v_flex()
                    .w_full()
                    .text_size(px(15.))
                    .line_height(px(25.))
                    .text_color(tokens.colors().text_primary)
                    .children((!formatted.is_empty()).then(|| {
                        TextView::markdown(("assistant", index), formatted.to_string())
                            .selectable(true)
                    }))
                    .children((!writing.is_empty()).then(|| div().child(writing.to_string())))
                    .into_any_element()
            }
            TranscriptBlock::Reasoning { text } => prose(text, tokens.colors().text_muted)
                .italic()
                .into_any_element(),
            TranscriptBlock::Tool {
                name,
                input,
                output,
                is_error,
                ..
            } => self.tool_card(name, input, output.as_deref(), *is_error, cx),
            TranscriptBlock::Question { question, options } => v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(tokens.colors().accent)
                        .child(question.clone()),
                )
                .children(options.iter().map(|option| {
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_secondary)
                        .child(format!("· {option}"))
                }))
                .into_any_element(),
            TranscriptBlock::Plan { plan } => {
                prose(plan, tokens.colors().text_secondary).into_any_element()
            }
            // A turn boundary is where a checkpoint was taken, which is what
            // makes it worth drawing — and what makes it the way back.
            TranscriptBlock::TurnEnd { turn } => self.turn_rule(*turn, cx),
            // A turn that worked says so by being answered. Only an outcome
            // the user has to do something about is worth a line of its own.
            TranscriptBlock::Outcome { state, summary } => match state {
                SessionState::Failed | SessionState::Cancelled => {
                    let colour = match state {
                        SessionState::Failed => tokens.colors().status_error,
                        _ => tokens.colors().text_muted,
                    };
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .rounded(px(tokens.radius.row))
                        .bg(tokens.colors().bg_surface)
                        .border_1()
                        .border_color(colour.opacity(0.4))
                        .text_sm()
                        .text_color(colour)
                        .child(match summary {
                            Some(summary) => summary.clone(),
                            None => state.as_str().to_string(),
                        })
                        .into_any_element()
                }
                _ => div().into_any_element(),
            },
        }
    }

    /// The rule between turns, and the way back to one.
    ///
    /// waku's idea, and the reason checkpoints exist: a position in a
    /// transcript maps to a working tree, so "put it back to how it was three
    /// turns ago" is an operation rather than an apology. Offered in two steps
    /// because it is destructive — work written since is removed — and a single
    /// click that quietly rewrites the worktree is not something to discover by
    /// accident. What it replaces is snapshotted first, so even a rewind the
    /// user regrets is reachable.
    fn turn_rule(&self, turn: u32, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let checkpoint = self
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.turn == turn)
            .map(|checkpoint| checkpoint.id.clone());
        let asking = self.rewinding == Some(turn);
        let rule = || {
            div()
                .h(px(1.))
                .flex_1()
                .bg(tokens.colors().border_subtle.opacity(0.6))
        };

        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(rule())
            .children(checkpoint.is_none().then(|| {
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted.opacity(0.7))
                    .child(rust_i18n::t!("transcript.turn", turn = turn).to_string())
            }))
            .children(checkpoint.clone().map(|id| {
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("turn-{turn}")))
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(tokens.radius.row))
                            .text_xs()
                            .text_color(if asking {
                                tokens.colors().text_primary
                            } else {
                                tokens.colors().text_muted.opacity(0.7)
                            })
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .tooltip(move |window, cx| {
                                Tooltip::new(rust_i18n::t!("transcript.rewind.hint").to_string())
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rewinding = (this.rewinding != Some(turn)).then_some(turn);
                                cx.notify();
                            }))
                            .child(if asking {
                                rust_i18n::t!("transcript.rewind.confirm").to_string()
                            } else {
                                rust_i18n::t!("transcript.turn", turn = turn).to_string()
                            }),
                    )
                    .children(asking.then(|| {
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                div()
                                    .id(SharedString::from(format!("rewind-{turn}")))
                                    .px(px(7.))
                                    .py(px(2.))
                                    .rounded(px(tokens.radius.row))
                                    .bg(tokens.colors().row_active())
                                    .text_xs()
                                    .text_color(tokens.colors().text_primary)
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.rewind(id.clone(), cx)
                                    }))
                                    .child(rust_i18n::t!("transcript.rewind.yes").to_string()),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("keep-{turn}")))
                                    .px(px(7.))
                                    .py(px(2.))
                                    .rounded(px(tokens.radius.row))
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.rewinding = None;
                                        cx.notify();
                                    }))
                                    .child(rust_i18n::t!("transcript.rewind.no").to_string()),
                            )
                    }))
            }))
            .child(rule())
            .into_any_element()
    }

    /// A tool call and, once it has one, its result.
    fn tool_card(
        &self,
        name: &str,
        input: &str,
        output: Option<&str>,
        is_error: bool,
        cx: &App,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let font = cx.theme_mono_font();
        v_flex()
            .w_full()
            .rounded(px(tokens.radius.row))
            .border_1()
            .border_color(if is_error {
                tokens.colors().status_error
            } else {
                tokens.colors().border_subtle
            })
            .bg(tokens.colors().bg_surface)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_secondary)
                            .child(if name.is_empty() {
                                rust_i18n::t!("transcript.tool").to_string()
                            } else {
                                name.to_string()
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .font_family(font.clone())
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(input.to_string()),
                    ),
            )
            .children(output.map(|output| {
                div()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .border_t_1()
                    .border_color(tokens.colors().border_subtle)
                    .font_family(font)
                    .text_xs()
                    .line_height(px(18.))
                    .text_color(if is_error {
                        tokens.colors().status_error
                    } else {
                        tokens.colors().text_secondary
                    })
                    // Bounded: a tool that printed a megabyte must not push the
                    // conversation off the screen.
                    .child(head_of(output, 24))
            }))
            .into_any_element()
    }

    /// The composer: a card holding the input, what will run it, and the way
    /// to send or stop it.
    ///
    /// One card rather than a row of controls beside a field. The prompt is
    /// the thing being written, so it gets the width; everything that decides
    /// what happens to it sits underneath, where it is legible without
    /// competing with the text.
    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Copied out: the pickers and chips below need `cx` mutably to bind
        // their listeners, and a borrow of the theme held across that is a
        // borrow held across the whole composer.
        let tokens = Tokens::global(cx).clone();
        let working = self.is_working();
        let picker = self.picker_panel(cx);
        let model_chip = self.model_chip_button(cx);
        let agent_chip = self.agent_chip_button(cx);
        let new_session = self.new_session_button(cx);

        v_flex()
            .w_full()
            .max_w(px(TRANSCRIPT_MEASURE))
            .mx_auto()
            .px_4()
            .pb_3()
            .gap_2()
            .children(picker)
            .child(
                v_flex()
                    .w_full()
                    .px(px(14.))
                    .py(px(9.))
                    .gap(px(7.))
                    .rounded(px(tokens.radius.card))
                    .bg(tokens.colors().bg_surface)
                    .border_1()
                    // The border carries the focus, so the field inside needs
                    // no chrome of its own.
                    .border_color(if self.composer_focused {
                        tokens.colors().accent.opacity(0.55)
                    } else {
                        tokens.colors().border_subtle
                    })
                    .child(Textarea::new(&self.composer))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::empty()
                                    .path(ginka_ui::assets::icon::PAPERCLIP)
                                    .size_4()
                                    .text_color(tokens.colors().text_muted),
                            )
                            .child(div().flex_1())
                            .children(model_chip)
                            .child(agent_chip)
                            .child(if working {
                                // An agent that cannot be stopped is one the
                                // user has to wait out. The composer keeps
                                // working: what is typed while it runs is
                                // queued, not lost.
                                div()
                                    .id("stop")
                                    .size(px(30.))
                                    .rounded_full()
                                    .bg(tokens.colors().bg_raised)
                                    .border_1()
                                    .border_color(tokens.colors().border_strong)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_active()))
                                    .tooltip(|window, cx| {
                                        Tooltip::new(rust_i18n::t!("composer.stop").to_string())
                                            .build(window, cx)
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                                    .child(
                                        div()
                                            .size(px(9.))
                                            .rounded(px(2.5))
                                            .bg(tokens.colors().text_primary),
                                    )
                                    .into_any_element()
                            } else {
                                div()
                                    .id("send")
                                    .size(px(30.))
                                    .rounded_full()
                                    .bg(tokens.colors().text_primary)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().accent))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                    )
                                    .child(
                                        Icon::new(IconName::ArrowUp)
                                            .size_4()
                                            .text_color(tokens.colors().bg_window),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(self.context_bar(cx)))
                    .children(new_session),
            )
    }

    /// The list the composer is offering, above the card.
    ///
    /// A panel rather than a menu: an agent's row has to say *why* it cannot be
    /// used, and "Claude Code — not signed in" is not a menu label. Choosing is
    /// still allowed — the user may be signing in in another window, and a
    /// picker that refuses the pick is not a picker.
    fn picker_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tokens = Tokens::global(cx);
        let picker = self.picker?;

        let rows: Vec<AnyElement> = match picker {
            Picker::Agent => self
                .agents
                .iter()
                .map(|agent| {
                    let id = agent.id.clone();
                    let chosen = self.chosen_agent.as_deref() == Some(agent.id.as_str());
                    let note = if !agent.installed {
                        Some(rust_i18n::t!("composer.agent.missing").to_string())
                    } else if agent.authenticated == Some(false) {
                        Some(rust_i18n::t!("composer.agent.signed_out").to_string())
                    } else {
                        agent.version.clone()
                    };
                    self.picker_row(
                        SharedString::from(format!("agent-option:{}", agent.id)),
                        agent.display_name.clone(),
                        note,
                        chosen,
                        cx.listener(move |this, _, _, cx| {
                            this.chosen_agent = Some(id.clone());
                            // The model belonged to the agent that was chosen
                            // before; it means nothing to the new one.
                            this.chosen_model = None;
                            this.picker = None;
                            cx.notify();
                        }),
                        cx,
                    )
                })
                .collect(),
            Picker::Command => self
                .commands
                .iter()
                .map(|command| {
                    let name = command.name.clone();
                    self.picker_row(
                        SharedString::from(format!("command:{}", command.name)),
                        format!("/{}", command.name),
                        Some(match &command.argument_hint {
                            Some(hint) => format!("{hint} · {}", command.description),
                            None => command.description.clone(),
                        }),
                        false,
                        cx.listener(move |this, _, window, cx| {
                            this.choose_command(&name.clone(), window, cx)
                        }),
                        cx,
                    )
                })
                .collect(),
            Picker::Mention => self
                .mentions
                .iter()
                .map(|file| {
                    let path = file.path.clone();
                    self.picker_row(
                        SharedString::from(format!("mention:{}", file.path)),
                        file.name.clone(),
                        // The directory, quietly, because two files with the
                        // same name are told apart by where they are.
                        file.path
                            .rsplit_once('/')
                            .map(|(directory, _)| directory.to_string()),
                        false,
                        cx.listener(move |this, _, window, cx| {
                            this.choose_mention(&path.clone(), window, cx)
                        }),
                        cx,
                    )
                })
                .collect(),
            Picker::Model => self
                .models()
                .into_iter()
                .map(|model| {
                    let picked = model.clone();
                    let chosen = self.chosen_model.as_deref() == Some(model.as_str());
                    self.picker_row(
                        SharedString::from(format!("model-option:{model}")),
                        model.clone(),
                        None,
                        chosen,
                        cx.listener(move |this, _, _, cx| {
                            this.chosen_model = Some(picked.clone());
                            this.picker = None;
                            cx.notify();
                        }),
                        cx,
                    )
                })
                .collect(),
        };

        if rows.is_empty() {
            return None;
        }

        Some(
            v_flex()
                .w_full()
                .p_1()
                .gap_0p5()
                .rounded(px(tokens.radius.card))
                .bg(tokens.colors().bg_surface)
                .border_1()
                .border_color(tokens.colors().border_subtle)
                .children(rows)
                .into_any_element(),
        )
    }

    /// One option in an open picker.
    fn picker_row(
        &self,
        id: impl Into<ElementId>,
        label: String,
        note: Option<String>,
        chosen: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .id(id)
            .w_full()
            .h(px(30.))
            .px(px(9.))
            .gap(px(8.))
            .items_center()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .when(chosen, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(on_click)
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(tokens.colors().text_primary)
                    .truncate()
                    .child(label),
            )
            .children(note.map(|note| {
                div()
                    .text_size(px(11.))
                    .text_color(tokens.colors().text_muted)
                    .child(note)
            }))
            .into_any_element()
    }

    /// The models the chosen agent offers, if it offers a choice.
    fn models(&self) -> Vec<String> {
        let chosen = self
            .session
            .as_ref()
            .map(|row| row.agent_to_start(&self.agents, self.chosen_agent.as_deref()));
        chosen
            .and_then(|id| {
                self.agents
                    .iter()
                    .find(|agent| agent.id == id)
                    .map(|agent| agent.models.clone())
            })
            .unwrap_or_default()
    }

    /// Put back what was being typed here when it was last left.
    fn load_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn_in(window, async move |this, cx| {
            let text = cx
                .background_spawn(async move { link.draft(&workspace).await })
                .await;
            if text.is_empty() {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                // Only into an empty composer: a draft arriving late must not
                // overwrite something the user has already started typing.
                if this.composer.read(cx).value().is_empty() {
                    this.composer
                        .update(cx, |state, cx| state.set_value(text, window, cx));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// What the composer does on every keystroke.
    ///
    /// Two things, both of which have to be cheap: keep the draft, so leaving
    /// does not lose it, and offer files while a mention is being typed.
    fn composer_changed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };

        // Written through the daemon rather than held here: a second window on
        // the same workspace is looking at the same draft.
        let link = self.link.clone();
        let saving = text.clone();
        let for_draft = workspace.clone();
        cx.background_spawn(async move { link.save_draft(&for_draft, saving).await })
            .detach();

        // A command takes the whole prompt, so it is asked about first.
        if let Some(query) = ginka_ui::transcript::command_being_typed(&text) {
            let query = query.to_string();
            let link = self.link.clone();
            let for_commands = workspace.clone();
            cx.spawn(async move |this, cx| {
                let found = cx
                    .background_spawn(async move { link.commands(&for_commands, &query).await })
                    .await;
                this.update(cx, |this, cx| {
                    let still =
                        ginka_ui::transcript::command_being_typed(&this.composer.read(cx).value())
                            .is_some();
                    this.commands = found;
                    this.picker = (still && !this.commands.is_empty()).then_some(Picker::Command);
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        if self.picker == Some(Picker::Command) {
            self.picker = None;
            self.commands.clear();
        }

        match ginka_ui::transcript::mention_being_typed(&text) {
            Some(query) => {
                let query = query.to_string();
                let link = self.link.clone();
                cx.spawn(async move |this, cx| {
                    let found = cx
                        .background_spawn(async move { link.files(&workspace, &query).await })
                        .await;
                    this.update(cx, |this, cx| {
                        // Only if a mention is still being typed: the answer
                        // may arrive after the user finished the word.
                        let still = ginka_ui::transcript::mention_being_typed(
                            &this.composer.read(cx).value(),
                        )
                        .is_some();
                        this.mentions = found;
                        this.picker = (still && !this.mentions.is_empty())
                            .then_some(Picker::Mention)
                            .or(match this.picker {
                                Some(Picker::Mention) => None,
                                other => other,
                            });
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            None => {
                if self.picker == Some(Picker::Mention) {
                    self.picker = None;
                    self.mentions.clear();
                    cx.notify();
                }
            }
        }
    }

    /// Put a chosen command into the prompt in place of what was typed.
    fn choose_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let completed = ginka_ui::transcript::complete_command(&text, name);
        self.composer
            .update(cx, |state, cx| state.set_value(completed, window, cx));
        self.picker = None;
        self.commands.clear();
        cx.notify();
    }

    /// Enter with the command picker open takes the best match.
    fn take_first_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(best) = self.commands.first().map(|command| command.name.clone()) else {
            return;
        };
        self.choose_command(&best, window, cx);
    }

    /// Put a chosen file into the prompt in place of what was typed.
    fn choose_mention(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let completed = ginka_ui::transcript::complete_mention(&text, path);
        self.composer
            .update(cx, |state, cx| state.set_value(completed, window, cx));
        self.picker = None;
        self.mentions.clear();
        cx.notify();
    }

    /// Enter with the mention picker open takes the best match.
    fn take_first_mention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(best) = self.mentions.first().map(|entry| entry.path.clone()) else {
            return;
        };
        self.choose_mention(&best, window, cx);
    }

    /// Open a picker, or close it if it is the one already open.
    fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.picker = (self.picker != Some(picker)).then_some(picker);
        cx.notify();
    }

    /// The agent chip, which is also how the agent is changed.
    fn agent_chip_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("agent-chip")
            .cursor_pointer()
            .tooltip(|window, cx| {
                Tooltip::new(rust_i18n::t!("composer.agent.pick").to_string()).build(window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(Picker::Agent, cx)))
            .child(self.agent_chip(cx))
    }

    /// The model chip, when the chosen agent offers a choice of models.
    ///
    /// Absent rather than empty for an agent that decides its own model:
    /// Codex resolves it from the user's own configuration, and a picker
    /// showing nothing would suggest something was broken.
    fn model_chip_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let models = self.models();
        if models.is_empty() {
            return None;
        }
        let tokens = Tokens::global(cx);
        let label = self
            .chosen_model
            .clone()
            .or_else(|| {
                self.session
                    .as_ref()
                    .and_then(|row| row.session.as_ref())
                    .and(None)
            })
            .unwrap_or_else(|| rust_i18n::t!("composer.model.default").to_string());

        Some(
            h_flex()
                .id("model-chip")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().row_hover())
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(Picker::Model, cx)))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(tokens.colors().text_secondary)
                        .child(label),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(12.))
                        .text_color(tokens.colors().text_muted),
                ),
        )
    }

    /// Which agent the composer would start, and whether it can be.
    ///
    /// The readiness is the point. An agent that is missing or signed out used
    /// to be discoverable only by sending a prompt and reading the failure it
    /// produced; saying so here is the difference between a tool that works
    /// and one that appears not to.
    fn agent_chip(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let chosen = self
            .session
            .as_ref()
            .map(|row| row.agent_to_start(&self.agents, self.chosen_agent.as_deref()));
        let status = chosen
            .as_ref()
            .and_then(|id| self.agents.iter().find(|agent| &agent.id == id));

        let (name, note, colour) = match (&chosen, status) {
            (None, _) => (
                rust_i18n::t!("composer.no_agent").to_string(),
                None,
                tokens.colors().text_muted,
            ),
            (Some(id), None) => (
                // Probed but unknown, or not probed yet.
                id.clone(),
                None,
                tokens.colors().text_secondary,
            ),
            (Some(_), Some(agent)) if !agent.installed => (
                agent.display_name.clone(),
                Some(rust_i18n::t!("composer.agent.missing").to_string()),
                tokens.colors().status_error,
            ),
            (Some(_), Some(agent)) if agent.authenticated == Some(false) => (
                agent.display_name.clone(),
                Some(rust_i18n::t!("composer.agent.signed_out").to_string()),
                tokens.colors().status_attention,
            ),
            (Some(_), Some(agent)) => (
                agent.display_name.clone(),
                None,
                tokens.colors().text_secondary,
            ),
        };

        h_flex()
            .h(px(28.))
            .px(px(9.))
            .gap(px(6.))
            .items_center()
            .rounded(px(tokens.radius.row))
            .bg(tokens.colors().row_hover())
            .children(
                self.session
                    .as_ref()
                    .map(|session| session.agent.glyph().size(px(14.)).text_color(colour)),
            )
            .child(div().text_size(px(12.)).text_color(colour).child(name))
            .children(note.map(|note| {
                div()
                    .text_size(px(12.))
                    .text_color(colour)
                    .child(format!("· {note}"))
            }))
    }

    /// The hairline strip under the composer: worktree left, branch right.
    /// Start the next prompt as a new conversation rather than a follow-up.
    ///
    /// Resuming is the right default — that is what a workspace's session is
    /// for — but a task that has nothing to do with the last one should not
    /// inherit its context, and the vendor charges for carrying it.
    fn new_session_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        self.session.as_ref()?.session.as_ref()?;
        let tokens = Tokens::global(cx).clone();
        Some(
            h_flex()
                .id("new-session")
                .h(px(22.))
                .px(px(7.))
                .gap(px(5.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .cursor_pointer()
                .when(self.start_fresh, |this| {
                    this.bg(tokens.colors().row_active())
                })
                .hover(|this| this.bg(tokens.colors().row_hover()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_fresh = !this.start_fresh;
                    cx.notify();
                }))
                .child(
                    Icon::new(IconName::Plus)
                        .size(px(11.))
                        .text_color(tokens.colors().text_muted),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(if self.start_fresh {
                            tokens.colors().text_secondary
                        } else {
                            tokens.colors().text_muted
                        })
                        .child(rust_i18n::t!("composer.new_session").to_string()),
                ),
        )
    }

    fn context_bar(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px(px(6.))
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
                            .text_size(px(11.))
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("composer.context.worktree").to_string()),
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
                            .text_size(px(11.))
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

    /// The terminal dock: a tab strip over the shell in front.
    ///
    /// One strip per workspace, because the shells are the workspace's: a
    /// build running in one worktree has nothing to do with the tab a reader
    /// has open in another.
    fn terminal_dock(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let active = self.terminals.active_index();

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
                    .children(
                        self.terminals
                            .tabs()
                            .iter()
                            .enumerate()
                            .map(|(index, tab)| {
                                let showing = index == active;
                                let id = tab.id.clone();
                                let closing = tab.id.clone();
                                h_flex()
                                    .id(SharedString::from(format!("terminal-tab:{}", tab.id)))
                                    .px_2()
                                    .py_1()
                                    .gap_2()
                                    .items_center()
                                    .rounded(px(tokens.radius.row))
                                    .when(showing, |this| this.bg(tokens.colors().row_active()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.show_terminal(index, window, cx)
                                    }))
                                    .child(
                                        Icon::new(IconName::SquareTerminal)
                                            .size_3()
                                            .text_color(tokens.colors().text_secondary),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if showing {
                                                tokens.colors().text_primary
                                            } else {
                                                tokens.colors().text_secondary
                                            })
                                            .child(tab.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("close-terminal:{id}")))
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.close_terminal(closing.clone(), cx)
                                            }))
                                            .child(
                                                Icon::new(IconName::Close)
                                                    .size_3()
                                                    .text_color(tokens.colors().text_muted),
                                            ),
                                    )
                            }),
                    )
                    .children((!self.terminals.is_empty()).then(|| {
                        div()
                            .id("new-terminal")
                            .px_1p5()
                            .py_1()
                            .rounded(px(tokens.radius.row))
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_terminal(window, cx)),
                            )
                            .child(
                                Icon::new(IconName::Plus)
                                    .size_3()
                                    .text_color(tokens.colors().text_muted),
                            )
                    }))
                    .child(div().flex_1()),
            )
            .child(match self.terminals.active() {
                Some(tab) => self.terminal_screen(&tab.screen, cx).into_any_element(),
                None => self.terminal_start(cx).into_any_element(),
            })
    }

    /// What the dock says before there is a shell in it.
    fn terminal_start(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        v_flex().flex_1().items_center().justify_center().child(
            div()
                .id("open-terminal")
                .px_3()
                .py_1p5()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().bg_surface)
                .border_1()
                .border_color(tokens.colors().border_subtle)
                .text_sm()
                .text_color(tokens.colors().text_secondary)
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_hover()))
                .on_click(cx.listener(|this, _, window, cx| this.open_terminal(window, cx)))
                .child(rust_i18n::t!("terminal.open").to_string()),
        )
    }

    /// The shell's screen, cell by cell.
    ///
    /// Every cell is drawn, blanks included: a shell that painted a bar across
    /// the width would otherwise lose its right-hand end, and a background
    /// colour would stop wherever the text did.
    fn terminal_screen(
        &self,
        screen: &ginka_ui::terminal::TerminalScreen,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = cx.theme_mono_font();
        let rows = screen.rows_of_cells();

        v_flex()
            .id("terminal-screen")
            .track_focus(&self.terminal_focus)
            .key_context("Terminal")
            .flex_1()
            .px_2()
            .py_1()
            .overflow_hidden()
            .font_family(mono)
            .text_size(px(12.5))
            .line_height(px(17.))
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.type_into_terminal(event, cx)),
            )
            .children(rows.into_iter().map(|row| {
                h_flex().children(row.into_iter().map(|cell| {
                    div()
                        .when(cell.cursor, |this| {
                            this.bg(tokens.colors().text_primary)
                                .text_color(tokens.colors().bg_terminal)
                        })
                        .when(!cell.cursor, |this| {
                            this.text_color(
                                cell.foreground
                                    .map(|colour| terminal_colour(colour, &tokens))
                                    .unwrap_or(tokens.colors().text_primary),
                            )
                            .when_some(cell.background, |this, colour| {
                                this.bg(terminal_colour(colour, &tokens))
                            })
                        })
                        .when(cell.bold, |this| this.font_semibold())
                        .when(cell.italic, |this| this.italic())
                        .child(if cell.text == ' ' {
                            // A space with no width is a hole in a painted bar.
                            SharedString::from("\u{00a0}")
                        } else {
                            SharedString::from(cell.text.to_string())
                        })
                }))
            }))
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

/// What a terminal's colour looks like in this theme.
///
/// The eight ANSI colours and their bright halves are resolved against the
/// theme's own palette rather than to fixed hexes: a terminal that hardcoded
/// them would clash with every theme but the one it was written against. What
/// a program asks for exactly — a truecolour escape — is given exactly.
fn terminal_colour(colour: ginka_ui::terminal::TerminalColor, tokens: &Tokens) -> Hsla {
    use ginka_ui::terminal::TerminalColor;
    let colors = tokens.colors();
    match colour {
        TerminalColor::Rgb(red, green, blue) => {
            gpui::rgb(((red as u32) << 16) | ((green as u32) << 8) | blue as u32).into()
        }
        TerminalColor::Named(index) => match index % 8 {
            0 => colors.text_muted,
            1 => colors.status_error,
            2 => colors.status_done,
            3 => colors.status_attention,
            4 => colors.accent,
            5 => colors.status_working,
            6 => colors.text_secondary,
            _ => colors.text_primary,
        },
    }
}

/// What a keystroke sends to a shell.
///
/// A pty takes bytes, so this is where a key becomes the bytes a terminal
/// expects: the control characters for `ctrl-`, the escape sequences for the
/// arrows and the editing keys, and the typed character otherwise. Anything
/// this does not know is not sent, because a wrong byte is worse than none.
fn keystroke_bytes(keystroke: &Keystroke) -> Option<String> {
    let key = keystroke.key.as_str();
    let modifiers = &keystroke.modifiers;

    if modifiers.control && key.len() == 1 {
        // ctrl-a is 0x01, and so on up the alphabet; ctrl-c is what stops a
        // runaway command, which is the whole reason this branch exists.
        let letter = key.chars().next()?.to_ascii_lowercase();
        if letter.is_ascii_lowercase() {
            return Some(((letter as u8 - b'a' + 1) as char).to_string());
        }
    }

    let sequence = match key {
        "enter" => "\r",
        "tab" => "\t",
        "backspace" => "\x7f",
        "escape" => "\x1b",
        "up" => "\x1b[A",
        "down" => "\x1b[B",
        "right" => "\x1b[C",
        "left" => "\x1b[D",
        "home" => "\x1b[H",
        "end" => "\x1b[F",
        "pageup" => "\x1b[5~",
        "pagedown" => "\x1b[6~",
        "delete" => "\x1b[3~",
        "space" => " ",
        _ => "",
    };
    if !sequence.is_empty() {
        return Some(sequence.to_string());
    }

    // What the keyboard actually produced, which is what carries the layout
    // and the shift state.
    keystroke.key_char.clone().filter(|typed| !typed.is_empty())
}

/// Re-read the workspaces and hand back the selected session, if any.
///
/// `Err` means the window has gone, which is how both loops know to stop.
async fn pull_rows(
    this: &WeakEntity<Shell>,
    link: &Arc<DaemonLink>,
    cx: &mut AsyncApp,
) -> Result<Option<SessionId>, ()> {
    let listing = link.clone();
    // A request to the daemon, which does the storage and one `git status` per
    // worktree: off the main thread, or the window stalls on every refresh.
    let (showing, wants_changes) = this
        .update(cx, |this, cx| {
            (
                this.session.as_ref().map(|row| row.workspace.clone()),
                // Only while the surface that shows them is open: reading a
                // diff runs git over the whole worktree, and a panel nobody
                // opened is not worth that on every tick.
                this.surfaces.read(cx).open_surface() == Some(ginka_ui::surface::Surface::Git),
            )
        })
        .map_err(|_| ())?;
    let (rows, agents, checkpoints, changes, staged, comments) = cx
        .background_spawn(async move {
            let rows = listing.workspaces(crate::daemon::now()).await;
            // Cached by the daemon, so this is a request rather than two
            // subprocesses per agent every tick.
            let agents = listing.agents().await;
            let checkpoints = match &showing {
                Some(workspace) => listing.checkpoints(workspace).await,
                None => Vec::new(),
            };
            let (changes, staged, comments) = match (&showing, wants_changes) {
                (Some(workspace), true) => (
                    listing
                        .changes(workspace, ginka_protocol::ChangeSource::Uncommitted)
                        .await,
                    listing.staged_paths(workspace).await,
                    listing.comments(workspace).await,
                ),
                _ => (None, Vec::new(), Vec::new()),
            };
            (rows, agents, checkpoints, changes, staged, comments)
        })
        .await;
    tracing::debug!(
        rows = rows.len(),
        agents = agents.len(),
        "refreshed workspaces"
    );
    this.update(cx, |this, cx| {
        this.agents = agents;
        this.checkpoints = checkpoints;
        if wants_changes {
            this.surfaces.update(cx, |surfaces, cx| {
                surfaces.set_changes(changes, cx);
                surfaces.set_staged(staged, cx);
                surfaces.set_comments(comments, cx);
            });
        }
        this.sidebar
            .update(cx, |sidebar, cx| sidebar.set_rows(rows, cx));
        this.session = this.sidebar.read(cx).selected_row().cloned();
        cx.notify();
        this.session.as_ref().and_then(|row| row.session.clone())
    })
    .map_err(|_| ())
}

/// Fold in whatever the session has said since this window last looked.
async fn pull_transcript(
    this: &WeakEntity<Shell>,
    link: &Arc<DaemonLink>,
    session: SessionId,
    cx: &mut AsyncApp,
) -> Result<(), ()> {
    let after = this
        .update(cx, |this, _| this.transcript_cursor(&session))
        .map_err(|_| ())?;
    let reading = link.clone();
    let asked = session.clone();
    let entries = cx
        .background_spawn(async move { reading.transcript(&asked, after).await })
        .await;
    this.update(cx, |this, cx| this.fold(&session, &entries, cx))
        .map_err(|_| ())
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything is measured: the tail on screen is whatever the
        // reveal has walked out so far.
        self.write_a_little_more(window);
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
            .on_action(cx.listener(Self::on_toggle_palette))
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
            .children(self.palette_view(cx))
    }
}

impl Shell {
    /// The palette itself: a box over the window with what matches under it.
    ///
    /// Over everything rather than in a column: it is the one control that is
    /// about the window rather than about what is in it.
    fn palette_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let tokens = Tokens::global(cx).clone();
        let found = self.palette_entries(cx);
        let chosen = palette.chosen;

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .child(
                    v_flex()
                        .id("palette")
                        .mt(px(120.))
                        .w(px(560.))
                        .max_h(px(420.))
                        .rounded(px(tokens.radius.card))
                        .bg(tokens.colors().bg_raised)
                        .border_1()
                        .border_color(tokens.colors().border_strong)
                        .shadow_lg()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            this.palette_key(event, window, cx)
                        }))
                        .child(
                            div()
                                .w_full()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(tokens.colors().border_subtle)
                                .child(Input::new(&palette.query)),
                        )
                        .child(
                            v_flex()
                                .id("palette-entries")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .py_1()
                                .children(found.iter().enumerate().map(|(index, entry)| {
                                    let command = entry.command.clone();
                                    h_flex()
                                        .id(SharedString::from(format!("palette:{}", entry.id)))
                                        .w_full()
                                        .px_3()
                                        .py_1p5()
                                        .gap_2()
                                        .items_center()
                                        .when(index == chosen, |this| {
                                            this.bg(tokens.colors().row_active())
                                        })
                                        .cursor_pointer()
                                        .hover(|this| this.bg(tokens.colors().row_hover()))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.palette = None;
                                            this.run_command(command.clone(), window, cx);
                                        }))
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_sm()
                                                .text_color(tokens.colors().text_primary)
                                                .truncate()
                                                .child(entry.label.clone()),
                                        )
                                        .children(entry.hint.clone().map(|hint| {
                                            div()
                                                .text_xs()
                                                .text_color(tokens.colors().text_muted)
                                                .child(hint)
                                        }))
                                }))
                                .children(found.is_empty().then(|| {
                                    div()
                                        .px_3()
                                        .py_2()
                                        .text_xs()
                                        .text_color(tokens.colors().text_muted)
                                        .child(rust_i18n::t!("palette.empty").to_string())
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// The palette's own state while it is open.
struct Palette {
    query: Entity<InputState>,
    /// What has been typed, kept here so the filter is not a read of the input
    /// on every frame.
    typed: String,
    /// Which entry Return would run.
    chosen: usize,
}
