//! The window shell: project/session navigation plus the conversation and
//! resizable surfaces of `docs/ui.md` §1.
//!
//! The centre column carries the transcript, the composer, the context bar and
//! the terminal dock. The transcript and composer are live: the composer starts
//! an agent in the selected workspace, or sends a follow-up to the session
//! already running there, and the transcript is folded from the daemon's event
//! stream. The terminal is still a placeholder (M3).

use crate::daemon::{DaemonLink, SessionLaunch};
use crate::sidebar::{SessionSidebar, SidebarEvent};
use crate::surfaces::SurfacePanel;
use ginka_core::Paths;
use ginka_core::settings::{self, AppSettings};
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::model::{AgentStatus, Attachment, Checkpoint, SessionState, TranscriptEntry};
use ginka_protocol::provider::ProviderModel;
use ginka_protocol::{ProjectName, SessionId, SubagentStepStatus, TaskStatus, WorkspaceId};
use ginka_ui::Tokens;
use ginka_ui::home;
use ginka_ui::layout::{
    HEADER_HEIGHT, Layout, PROJECT_RAIL_WIDTH, Panel, TRAFFIC_LIGHT_INSET, navigator_width,
};
use ginka_ui::navigation::NavigationHistory;
use ginka_ui::transcript::{
    Activity, Applied, Block as TranscriptBlock, Reveal, Transcript, head_of,
};
use ginka_ui::workspace::{
    ProjectDraftError, ProjectRow, SessionRow, validate_project_draft, workspace_for_new_chat,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_base::TextSelection;
use gpui_component::text::TextView;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    Disableable as _, ElementExt as _, Icon, IconName, InteractiveElementExt as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, Paste, Textarea, TextareaState},
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    scroll::ScrollableElement as _,
    v_flex,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

actions!(
    shell,
    [
        ToggleSidebar,
        ToggleRightPanel,
        ToggleTerminalDock,
        TogglePalette,
        FindTranscript,
        NextSurface,
        PreviousSurface,
        NextTerminalTab,
        PreviousTerminalTab,
        CopyTerminalOutput,
        NavigateBack,
        NavigateForward
    ]
);

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = shell, no_json)]
struct SwitchSession(usize);

/// One place the title-bar history can revisit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NavigationTarget {
    /// The new-chat home, optionally aimed at a registered project.
    Home(Option<ProjectName>),
    /// A workspace and its current conversation.
    Workspace(WorkspaceId),
}

/// What the composer is offering a choice of.
///
/// One at a time: two open lists over a conversation is a menu, not a
/// composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picker {
    /// Which project — or none at all — the next chat runs in.
    Project,
    /// Which local branch the current workspace has checked out.
    Branch,
    /// Which agent runs the next prompt.
    Agent,
    /// Which of that agent's models it runs on.
    Model,
    /// How much reasoning the selected model uses.
    ReasoningEffort,
    /// Which provider service tier the selected model uses.
    ServiceTier,
    /// Which of that agent's logins it runs on (`docs/accounts.md` §11).
    Account,
    /// What the agent may touch (`docs/roadmap.md` §3.3 N2).
    Access,
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
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindTranscript, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindTranscript, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-right", NextSurface, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-right", NextSurface, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-left", PreviousSurface, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-left", PreviousSurface, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-]", NextTerminalTab, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-pagedown", NextTerminalTab, Some("Terminal")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-[", PreviousTerminalTab, Some("Terminal")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-pageup", PreviousTerminalTab, Some("Terminal")),
        KeyBinding::new(
            ginka_ui::terminal::paste_shortcut(cfg!(target_os = "macos")),
            Paste,
            Some("Terminal"),
        ),
        KeyBinding::new(
            ginka_ui::terminal::copy_shortcut(cfg!(target_os = "macos")),
            CopyTerminalOutput,
            Some("Terminal"),
        ),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-[", NavigateBack, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-up", NavigateBack, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-]", NavigateForward, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-down", NavigateForward, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-1", SwitchSession(0), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-2", SwitchSession(1), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-3", SwitchSession(2), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-4", SwitchSession(3), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-5", SwitchSession(4), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-6", SwitchSession(5), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-7", SwitchSession(6), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-8", SwitchSession(7), Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-9", SwitchSession(8), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-1", SwitchSession(0), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-2", SwitchSession(1), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-3", SwitchSession(2), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-4", SwitchSession(3), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-5", SwitchSession(4), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-6", SwitchSession(5), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-7", SwitchSession(6), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-8", SwitchSession(7), Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-9", SwitchSession(8), Some(CONTEXT)),
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

/// Width of one 12.5 px monospace cell in the terminal grid.
const TERMINAL_CELL_WIDTH: f32 = 7.5;

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

/// The turn-level handoff choice currently expanded in the transcript.
struct ForkMenu {
    turn: u32,
    seq: u64,
    busy: bool,
    error: Option<String>,
}

/// Find-in-page state for the open persisted conversation.
struct TranscriptSearch {
    query: Entity<InputState>,
    typed: String,
    matches: Vec<ginka_protocol::model::SessionMatch>,
    chosen: Option<usize>,
    loading: bool,
    error: Option<String>,
}

/// Find state for one daemon-owned terminal tab.
struct TerminalSearch {
    terminal: ginka_protocol::TerminalId,
    query: Entity<InputState>,
    matches: Vec<ginka_ui::terminal::TerminalSearchMatch>,
    chosen: Option<usize>,
}

/// A mouse selection in one visible daemon terminal.
#[derive(Clone)]
struct TerminalDrag {
    terminal: ginka_protocol::TerminalId,
    selection: ginka_ui::terminal::TerminalSelection,
}

/// One daemon-owned upload waiting in the composer, plus an optional local thumbnail.
struct ComposerAttachment {
    attachment: Attachment,
    preview_url: Option<String>,
}

/// One image being annotated before it is returned to the composer.
struct ImageMarkup {
    source_reference: String,
    source_name: String,
    preview_url: String,
    document: ginka_ui::markup::MarkupDocument,
    tool: ginka_ui::markup::MarkupTool,
    canvas_bounds: Bounds<Pixels>,
    drawing: bool,
    text: Entity<InputState>,
}

/// Upload prepared attachment bytes and build the local presentation rows.
async fn upload_attachment_payloads(
    link: Arc<DaemonLink>,
    payloads: Vec<(String, Vec<u8>)>,
) -> Result<Vec<ComposerAttachment>, String> {
    let mut uploaded = Vec::with_capacity(payloads.len());
    for (name, bytes) in payloads {
        let preview_url = ginka_core::files::preview_image(&bytes)
            .map(|image| format!("data:{};base64,{}", image.media_type, image.data_base64));
        let attachment = link.upload_attachment(name, bytes).await?;
        uploaded.push(ComposerAttachment {
            attachment,
            preview_url,
        });
    }
    Ok(uploaded)
}

fn paint_markup_shape(
    shape: &ginka_ui::markup::MarkupShape,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    use ginka_ui::markup::MarkupShape;
    let at = |value: ginka_ui::markup::Point| {
        point(bounds.origin.x + px(value.x), bounds.origin.y + px(value.y))
    };
    let stroke = match shape {
        MarkupShape::Highlight(_) => (px(14.), rgb(0xffd84d).alpha(0.42)),
        _ => (px(3.), rgb(0xff4d67)),
    };
    let mut builder = PathBuilder::stroke(stroke.0);
    match shape {
        MarkupShape::Pen(points) | MarkupShape::Highlight(points) => {
            for (index, value) in points.iter().enumerate() {
                if index == 0 {
                    builder.move_to(at(*value));
                } else {
                    builder.line_to(at(*value));
                }
            }
        }
        MarkupShape::Arrow([start, end]) => {
            builder.move_to(at(*start));
            builder.line_to(at(*end));
        }
        MarkupShape::Rectangle([start, end]) => {
            let left = start.x.min(end.x);
            let right = start.x.max(end.x);
            let top = start.y.min(end.y);
            let bottom = start.y.max(end.y);
            builder.move_to(at(ginka_ui::markup::Point::new(left, top)));
            builder.line_to(at(ginka_ui::markup::Point::new(right, top)));
            builder.line_to(at(ginka_ui::markup::Point::new(right, bottom)));
            builder.line_to(at(ginka_ui::markup::Point::new(left, bottom)));
            builder.close();
        }
        MarkupShape::Ellipse([start, end]) => {
            let left = start.x.min(end.x);
            let right = start.x.max(end.x);
            let top = start.y.min(end.y);
            let bottom = start.y.max(end.y);
            let radius_x = px((right - left) / 2.0);
            let radius_y = px((bottom - top) / 2.0);
            builder.move_to(at(ginka_ui::markup::Point::new(
                right,
                (top + bottom) / 2.0,
            )));
            builder.arc_to(
                point(radius_x, radius_y),
                px(0.),
                false,
                false,
                at(ginka_ui::markup::Point::new(left, (top + bottom) / 2.0)),
            );
            builder.arc_to(
                point(radius_x, radius_y),
                px(0.),
                false,
                false,
                at(ginka_ui::markup::Point::new(right, (top + bottom) / 2.0)),
            );
            builder.close();
        }
        MarkupShape::Text { at: anchor, .. } => {
            let anchor = at(*anchor);
            builder.move_to(anchor + point(px(-5.), px(0.)));
            builder.line_to(anchor + point(px(5.), px(0.)));
            builder.move_to(anchor + point(px(0.), px(-5.)));
            builder.line_to(anchor + point(px(0.), px(5.)));
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, stroke.1);
    }
}

fn markup_tool_label(tool: ginka_ui::markup::MarkupTool) -> String {
    use ginka_ui::markup::MarkupTool;
    match tool {
        MarkupTool::Pen => rust_i18n::t!("composer.markup.pen").to_string(),
        MarkupTool::Highlight => rust_i18n::t!("composer.markup.highlight").to_string(),
        MarkupTool::Arrow => rust_i18n::t!("composer.markup.arrow").to_string(),
        MarkupTool::Rectangle => rust_i18n::t!("composer.markup.rectangle").to_string(),
        MarkupTool::Ellipse => rust_i18n::t!("composer.markup.ellipse").to_string(),
        MarkupTool::Text => rust_i18n::t!("composer.markup.text").to_string(),
    }
}

pub struct Shell {
    /// Where `app.json` lives, so a toggle can be written straight back.
    paths: Paths,
    /// Whether daemon-host paths are paths on this window's machine too.
    local_paths: bool,
    settings: AppSettings,
    layout: Layout,
    /// The row the centre column is showing.
    ///
    /// `None` is the home screen: nothing registered yet, or a new chat that
    /// has not been sent. Neither is an error — the window opens on it, and
    /// the first prompt is what turns it into a conversation.
    session: Option<SessionRow>,
    /// Every registered project, for the sidebar's headings and the
    /// composer's project chip.
    projects: Vec<ProjectRow>,
    /// Where a chat that has no workspace yet would run. `None` means no
    /// project, which is a scratch worktree rather than nowhere.
    target_project: Option<ProjectName>,
    /// A project-search hit to open after the sidebar finishes switching workspaces.
    pending_file_open: Option<(WorkspaceId, String)>,
    /// Everything the app knows is behind this.
    link: Arc<crate::daemon::DaemonLink>,
    /// The selected session's transcript, folded from the daemon's events.
    transcript: Transcript,
    /// Which session the transcript belongs to, so switching rows replaces it
    /// rather than appending one conversation to another.
    transcript_of: Option<SessionId>,
    /// Present while the reader is finding text in the open conversation.
    transcript_search: Option<TranscriptSearch>,
    /// Whether the turn-sized prompt navigator is expanded above the transcript.
    prompt_outline_open: bool,
    /// What each agent CLI on this machine says about itself, so the composer
    /// can say which agent it would start and whether it will work.
    agents: Vec<AgentStatus>,
    /// Every login of every provider, as the daemon lists them.
    accounts: Vec<ginka_protocol::model::Account>,
    /// The latest reading of each login's rate-limit windows, followed from
    /// the daemon's pushes so a chip moves when a turn moves the gauge.
    plans: Vec<ginka_protocol::model::PlanSnapshot>,
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
    /// Whether a press landed on a header strip and has not been released.
    ///
    /// With no title bar the strips are what the window is dragged by, and a
    /// drag is a press that then moved: acting on the press alone would move
    /// the window whenever a control on the strip was clicked.
    dragging: bool,
    /// Which picker is open above the composer, if any.
    picker: Option<Picker>,
    /// The files offered for the mention being typed, if one is.
    mentions: Vec<ginka_protocol::model::FileEntry>,
    /// The commands offered for the `/` being typed, if one is.
    commands: Vec<ginka_protocol::model::SlashCommand>,
    /// Follow-ups waiting behind the selected session's active turn.
    queued_messages: Vec<ginka_protocol::model::QueuedMessage>,
    /// Whether the active transport can accept a waiting prompt immediately.
    queue_can_send_now: bool,
    /// Stable id of the queued prompt currently being edited in the composer.
    editing_queued_message: Option<u64>,
    /// The ordinary draft displaced while a queued prompt is edited.
    queue_edit_draft: Option<String>,
    /// Why the most recent queue mutation was refused.
    queue_error: Option<String>,
    /// Files already copied into the daemon store for the next ordinary prompt.
    attachments: Vec<ComposerAttachment>,
    /// Full-size annotation dialog for one image attachment.
    image_markup: Option<ImageMarkup>,
    /// Whether selected files are currently being read and uploaded.
    attachment_busy: bool,
    /// Why the most recent selected file could not be attached.
    attachment_error: Option<String>,
    /// Invalidates an upload reply when navigation changed underneath it.
    attachment_generation: u64,
    /// The agent the user chose, which beats whatever would have been picked
    /// for them. `None` until they choose one.
    chosen_agent: Option<String>,
    /// The model the user chose for that agent.
    chosen_model: Option<String>,
    /// The reasoning level chosen for that model.
    chosen_reasoning_effort: Option<String>,
    /// The service tier chosen for that model.
    chosen_service_tier: Option<String>,
    /// What the next session's agent may touch. `None` is the daemon's
    /// default, `ask`. Fixed for a conversation once it has started, so the
    /// chip is only offered where a new session is about to be.
    chosen_access: Option<ginka_protocol::AccessMode>,
    /// The login the user chose for that agent. `None` runs the provider's
    /// default, which is what one login per provider always is.
    chosen_account: Option<ginka_protocol::AccountId>,
    /// The add-login dialog, while it is open.
    add_account: Option<AddAccount>,
    /// The add-project dialog, while it is open.
    add_project: Option<AddProject>,
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
    /// A turn whose conversation can be continued by another agent.
    forking: Option<ForkMenu>,
    /// Where keystrokes go when the terminal has the keyboard.
    terminal_focus: FocusHandle,
    /// Present while the reader is finding text in the active terminal.
    terminal_search: Option<TerminalSearch>,
    /// The visible terminal cells selected by the latest mouse drag.
    terminal_selection: Option<TerminalDrag>,
    /// Last painted bounds of each visible terminal pane.
    terminal_bounds: HashMap<ginka_protocol::TerminalId, Bounds<Pixels>>,
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
    /// Project/session visits addressed by the title-bar arrows.
    navigation: NavigationHistory<NavigationTarget>,
    composer: Entity<TextareaState>,
    /// Search text for the model catalogue popover.
    model_query: Entity<InputState>,
    /// A copied query keeps filtering in the testable `ginka-ui` layer.
    model_filter: String,
    /// Search or new-branch text in the branch popover.
    branch_query: Entity<InputState>,
    /// A copied query keeps branch filtering in the testable `ginka-ui` layer.
    branch_filter: String,
    /// The branches most recently read from git through the daemon.
    branches: Vec<ginka_protocol::model::BranchInfo>,
    /// A branch listing or checkout refusal shown in the open popover.
    branch_error: Option<String>,
    /// Set while branch state is being read or changed.
    branch_busy: bool,
    /// Set while the daemon is opening the semantic-index terminal.
    index_starting: bool,
    /// Why the most recent indexing request could not start.
    index_error: Option<String>,
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
        let local_paths = link.allows_local_paths();
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
        let sidebar_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("sidebar.sessions.search").to_string())
        });
        let sidebar = cx.new(|_| SessionSidebar::new(rows, sidebar_search.clone(), local_paths));
        let surfaces = cx.new(|cx| SurfacePanel::new(window, cx, local_paths));

        let sidebar_search_changed =
            cx.subscribe(&sidebar_search, |this, query, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = query.read(cx).value().to_string();
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.set_search_query(value, cx));
                }
            });

        let committing = cx.subscribe_in(
            &surfaces,
            window,
            |this, _, event, window, cx| match event {
                crate::surfaces::SurfaceEvent::SurfaceShown(surface) => {
                    this.persist_surface(*surface)
                }
                crate::surfaces::SurfaceEvent::Commit {
                    message,
                    only_staged,
                } => this.commit(message.clone(), *only_staged, cx),
                crate::surfaces::SurfaceEvent::Comment { path, line, text } => {
                    this.leave_comment(path.clone(), *line, text.clone(), cx)
                }
                crate::surfaces::SurfaceEvent::SendReview => this.send_review(cx),
                crate::surfaces::SurfaceEvent::GenerateCommitMessage { only_staged } => {
                    this.generate_commit_message(*only_staged, cx)
                }
                crate::surfaces::SurfaceEvent::Pull => this.sync_git(false, cx),
                crate::surfaces::SurfaceEvent::Push => this.sync_git(true, cx),
                crate::surfaces::SurfaceEvent::RefreshHistory => this.refresh_history(cx),
                crate::surfaces::SurfaceEvent::Stage { path, staged } => {
                    this.stage(path.clone(), *staged, cx)
                }
                crate::surfaces::SurfaceEvent::StageHunk {
                    path,
                    header,
                    staged,
                } => this.stage_hunk(path.clone(), header.clone(), *staged, cx),
                crate::surfaces::SurfaceEvent::RevertHunk { path, header } => {
                    this.revert_hunk(path.clone(), header.clone(), cx)
                }
                crate::surfaces::SurfaceEvent::Revert { path } => this.revert(path.clone(), cx),
                crate::surfaces::SurfaceEvent::FindFiles(query) => {
                    this.find_files(query.clone(), cx)
                }
                crate::surfaces::SurfaceEvent::OpenFile(path) => {
                    this.open_file(path.clone(), false, window, cx)
                }
                crate::surfaces::SurfaceEvent::OpenWorkspaceFile { workspace, path } => {
                    this.open_workspace_file(workspace.clone(), path.clone(), window, cx)
                }
                crate::surfaces::SurfaceEvent::OpenDefinition { workspace, target } => {
                    this.open_definition(workspace, target.clone(), window, cx)
                }
                crate::surfaces::SurfaceEvent::OpenFileFromHistory(path) => {
                    this.open_file(path.clone(), true, window, cx)
                }
                crate::surfaces::SurfaceEvent::SaveFile {
                    workspace,
                    path,
                    text,
                    expected_revision,
                } => this.save_file(
                    workspace.clone(),
                    path.clone(),
                    text.clone(),
                    expected_revision.clone(),
                    cx,
                ),
                crate::surfaces::SurfaceEvent::AddFileReference(reference) => {
                    this.add_file_reference(reference, window, cx)
                }
                crate::surfaces::SurfaceEvent::AddBrowserContext(context) => {
                    this.add_browser_context(context, window, cx)
                }
                crate::surfaces::SurfaceEvent::WriteTerminalSelection(selection) => {
                    this.write_terminal_selection(selection.clone(), window, cx)
                }
                crate::surfaces::SurfaceEvent::RefreshSkills => this.refresh_skills(cx),
                crate::surfaces::SurfaceEvent::SetSkillEnabled { name, enabled } => {
                    this.set_skill_enabled(name.clone(), *enabled, cx)
                }
            },
        );

        let selection =
            cx.subscribe_in(
                &sidebar,
                window,
                |this, sidebar, event, window, cx| match event {
                    // The window's own way in; see `add_project`.
                    SidebarEvent::AddProjectRequested => this.open_add_project(window, cx),
                    // A new conversation in whatever is selected: the project
                    // itself, the project of the selected workspace, or none
                    // at all. The column clears and the next message opens a
                    // session rather than continuing the last one.
                    SidebarEvent::NewChatRequested => {
                        let project = sidebar.read(cx).selected_project().cloned();
                        this.start_new_chat(project, window, cx);
                    }
                    // A project was picked and nothing under it. There is no
                    // conversation to show for that, which is the point: the
                    // home screen, aimed at this project.
                    SidebarEvent::ProjectSelected => {
                        let project = sidebar.read(cx).selected_project().cloned();
                        this.start_new_chat(project, window, cx);
                    }
                    SidebarEvent::Selected => {
                        let selected = sidebar.read(cx).selected_row().cloned();
                        let changed_workspace = this.session.as_ref().map(|row| &row.workspace)
                            != selected.as_ref().map(|row| &row.workspace);
                        if changed_workspace {
                            this.remember_workspace_view(cx);
                        }
                        this.session = selected;
                        if let Some(workspace) =
                            this.session.as_ref().map(|row| row.workspace.clone())
                        {
                            this.navigation
                                .visit(NavigationTarget::Workspace(workspace));
                        }
                        if changed_workspace {
                            if let Some(workspace) =
                                this.session.as_ref().map(|row| row.workspace.clone())
                            {
                                this.restore_workspace_view(&workspace, cx);
                            }
                            this.surfaces
                                .update(cx, |surfaces, cx| surfaces.clear_file(cx));
                            this.save_settings();
                        }
                        // A row says which project it is in, and the composer's
                        // chip and the next new chat both read that back.
                        this.target_project = sidebar.read(cx).selected_project().cloned();
                        // A different workspace is a different conversation.
                        this.transcript = Transcript::new();
                        this.transcript_of = None;
                        this.transcript_search = None;
                        this.session_state = None;
                        this.submitted = false;
                        this.transcript_follows = true;
                        this.picker = None;
                        this.attachments.clear();
                        this.image_markup = None;
                        this.surfaces
                            .update(cx, |surfaces, cx| surfaces.suspend_browser(false, cx));
                        this.attachment_busy = false;
                        this.attachment_error = None;
                        this.attachment_generation = this.attachment_generation.wrapping_add(1);
                        this.chosen_agent = None;
                        this.chosen_model = None;
                        this.chosen_reasoning_effort = None;
                        this.chosen_service_tier = None;
                        this.chosen_account = None;
                        this.start_fresh = false;
                        this.checkpoints = Vec::new();
                        this.rewinding = None;
                        this.forking = None;
                        this.index_starting = false;
                        this.index_error = None;
                        // The shells belong to the workspace, not to the
                        // window: a different workspace is a different strip.
                        this.terminal_search = None;
                        this.terminals = ginka_ui::terminal::TerminalTabs::new();
                        if this.layout.is_open(Panel::TerminalDock) {
                            this.adopt_terminals(cx);
                        }
                        this.mentions.clear();
                        this.queued_messages.clear();
                        this.queue_can_send_now = false;
                        this.editing_queued_message = None;
                        this.queue_edit_draft = None;
                        this.queue_error = None;
                        // Whatever was half-written here when it was last left.
                        this.composer
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.load_draft(window, cx);
                        this.refresh_queue(cx);
                        if this.surfaces.read(cx).open_surface()
                            == Some(ginka_ui::surface::Surface::Skills)
                        {
                            this.refresh_skills(cx);
                        }
                        if changed_workspace
                            && this.surfaces.read(cx).open_surface()
                                == Some(ginka_ui::surface::Surface::Files)
                        {
                            this.find_files(String::new(), cx);
                        }
                        if let Some((workspace, path)) = this.pending_file_open.take() {
                            if this.session.as_ref().map(|row| &row.workspace) == Some(&workspace) {
                                this.open_file(path, false, window, cx);
                            } else {
                                this.pending_file_open = Some((workspace, path));
                            }
                        }
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
        let model_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("composer.model.search").to_string())
        });
        let model_query_changed =
            cx.subscribe(&model_query, |this, query, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.model_filter = query.read(cx).value().to_string();
                    cx.notify();
                }
            });
        let branch_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("composer.branch.search").to_string())
                .submit_on_enter(true)
        });
        let branch_query_changed = cx.subscribe(
            &branch_query,
            |this, query, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.branch_filter = query.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { shift: false, .. } => this.take_branch_choice(cx),
                _ => {}
            },
        );
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
                        DaemonEvent::SessionQueueChanged { session } => {
                            let is_showing = this
                                .update(cx, |this, _| {
                                    this.session.as_ref().and_then(|row| row.session.as_ref())
                                        == Some(&session)
                                })
                                .unwrap_or(false);
                            if is_showing {
                                pull_queue(&this, &link, session, cx).await
                            } else {
                                Ok(())
                            }
                        }
                        // Anything that changes what the sidebar says.
                        DaemonEvent::ProjectsChanged
                        | DaemonEvent::WorkspacesChanged { .. }
                        | DaemonEvent::WorkspaceStatusChanged { .. }
                        | DaemonEvent::SessionStarted { .. }
                        | DaemonEvent::SessionOptionsChanged { .. } => {
                            pull_rows(&this, &link, cx).await.map(|_| ())
                        }
                        // A gauge moved. Straight onto the chip: the turn
                        // that moved it is the one the reader is watching.
                        DaemonEvent::PlanUsageChanged { snapshot } => this
                            .update(cx, |this, cx| {
                                this.plans.retain(|known| known.account != snapshot.account);
                                this.plans.push(snapshot.clone());
                                this.surfaces
                                    .update(cx, |surfaces, cx| surfaces.set_plan(snapshot, cx));
                                this.sync_footer(cx);
                                cx.notify();
                            })
                            .map_err(|_| ()),
                        // A login was added, removed or signed in: the list
                        // is re-read with the rest.
                        DaemonEvent::AccountsChanged => {
                            match pull_rows(&this, &link, cx).await {
                                Err(()) => Err(()),
                                Ok(_) => this
                                    .update(cx, |this, cx| {
                                        // The daemon's active marker is
                                        // authoritative, including when
                                        // another client made the choice.
                                        this.chosen_account = None;
                                        this.sync_footer(cx);
                                        cx.notify();
                                    })
                                    .map_err(|_| ()),
                            }
                        }
                        // The agent's commit message, for the box that asked
                        // for it — if it is still the workspace on screen.
                        DaemonEvent::CommitMessageGenerated {
                            workspace,
                            message,
                            error,
                        } => this
                            .update(cx, |this, cx| {
                                if this.session.as_ref().map(|row| &row.workspace)
                                    == Some(&workspace)
                                {
                                    this.surfaces.update(cx, |surfaces, cx| {
                                        surfaces.set_generated(message, error, cx)
                                    });
                                }
                            })
                            .map_err(|_| ()),
                        // The shell printed something. Straight onto its
                        // screen: a terminal that lagged behind what was typed
                        // into it would be unusable for the thing terminals
                        // are for.
                        DaemonEvent::TerminalOutput { terminal, data } => this
                            .update(cx, |this, cx| {
                                if this.terminals.feed(&terminal, &data) {
                                    this.refresh_terminal_search(&terminal, cx);
                                    cx.notify();
                                }
                            })
                            .map_err(|_| ()),
                        DaemonEvent::TerminalClosed { terminal } => this
                            .update(cx, |this, cx| {
                                let was_split = this.terminals.split_ids().is_some();
                                if this
                                    .terminal_search
                                    .as_ref()
                                    .is_some_and(|search| search.terminal == terminal)
                                {
                                    this.terminal_search = None;
                                }
                                this.terminals.close(&terminal);
                                if was_split && this.terminals.split_ids().is_none() {
                                    this.resize_terminal(cx);
                                }
                                this.persist();
                                cx.notify();
                            })
                            .map_err(|_| ()),
                        // A chat connector came or went. Nothing in the
                        // window draws one yet; `ginka slack status` does.
                        DaemonEvent::ConnectorStateChanged { .. } => Ok(()),
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
            this.refresh_queue(cx);
        });
        Self {
            layout: Layout::from_settings(&settings),
            link,
            transcript: Transcript::new(),
            transcript_of: None,
            transcript_search: None,
            prompt_outline_open: false,
            agents: Vec::new(),
            accounts: Vec::new(),
            plans: Vec::new(),
            reveal: Reveal::new(),
            palette: None,
            terminal_focus: cx.focus_handle(),
            terminal_search: None,
            terminal_selection: None,
            terminal_bounds: HashMap::new(),
            terminals: ginka_ui::terminal::TerminalTabs::new(),
            session_state: None,
            submitted: false,
            composer_focused: false,
            dragging: false,
            picker: None,
            mentions: Vec::new(),
            commands: Vec::new(),
            queued_messages: Vec::new(),
            queue_can_send_now: false,
            editing_queued_message: None,
            queue_edit_draft: None,
            queue_error: None,
            attachments: Vec::new(),
            image_markup: None,
            attachment_busy: false,
            attachment_error: None,
            attachment_generation: 0,
            chosen_agent: None,
            chosen_model: None,
            chosen_reasoning_effort: None,
            chosen_service_tier: None,
            chosen_access: None,
            chosen_account: None,
            add_account: None,
            add_project: None,
            projects: Vec::new(),
            target_project: None,
            pending_file_open: None,
            start_fresh: false,
            checkpoints: Vec::new(),
            rewinding: None,
            forking: None,
            transcript_scroll: ScrollHandle::new(),
            transcript_follows: true,
            navigation: NavigationHistory::new(NavigationTarget::Home(None)),
            composer,
            model_query,
            model_filter: String::new(),
            branch_query,
            branch_filter: String::new(),
            branches: Vec::new(),
            branch_error: None,
            branch_busy: false,
            index_starting: false,
            index_error: None,
            paths,
            local_paths,
            settings,
            session,
            sidebar,
            surfaces,
            _subscriptions: vec![
                appearance,
                selection,
                submitted,
                committing,
                model_query_changed,
                branch_query_changed,
                sidebar_search_changed,
            ],
        }
    }

    /// Register a repository or a folder, and aim the next chat at it.
    ///
    /// The window's own way in. The CLI remains available separately, but a
    /// reader who has just opened the app should not have to leave it to put
    /// something in it. The path is one
    /// this window picked, so it is a path on *this* machine — only the same
    /// thing as a daemon-host path while the daemon was discovered or spawned locally
    /// (`docs/roadmap.md` §4.1), which is why the picker is offered only then.
    fn open_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.local_paths {
            return;
        }
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("project.add.name.placeholder").to_string())
        });
        let name_changed = cx.subscribe(&name, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        name.read(cx).focus_handle(cx).focus(window, cx);
        self.add_project = Some(AddProject {
            name,
            path: None,
            error: None,
            busy: false,
            _name_changed: name_changed,
        });
        cx.notify();
    }

    /// Ask the platform for the source folder while keeping the project dialog open.
    fn choose_project_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            // A project is a directory: a repository, or a folder to work in.
            directories: true,
            multiple: false,
            prompt: Some(
                rust_i18n::t!("project.add.source.choose")
                    .to_string()
                    .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            // Cancelled, or the platform refused to ask: either way there is
            // nothing to register.
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let Some(dialog) = this.add_project.as_mut() else {
                    return;
                };
                if dialog.name.read(cx).value().trim().is_empty()
                    && let Some(folder) = path.file_name().and_then(|name| name.to_str())
                {
                    dialog
                        .name
                        .update(cx, |name, cx| name.set_value(folder, window, cx));
                }
                dialog.path = Some(path);
                dialog.error = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Pick local files and copy them into the daemon-owned attachment store.
    ///
    /// Paths never enter the transcript. Only the returned opaque references
    /// are kept by the window, so this remains valid when the daemon runs on a
    /// different host from the client.
    fn choose_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !ginka_ui::composer::can_accept_attachments(
            self.attachment_busy,
            self.session_state == Some(SessionState::AwaitingInput),
        ) {
            return;
        }
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(
                rust_i18n::t!("composer.attachment.choose")
                    .to_string()
                    .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            this.update(cx, |this, cx| this.attach_paths(paths, cx))
                .ok();
        })
        .detach();
    }

    /// Read local files and copy them into the daemon-owned attachment store.
    ///
    /// The picker and OS drag-and-drop converge here, so both paths get the
    /// same preview, error and stale-navigation behaviour.
    fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty()
            || !ginka_ui::composer::can_accept_attachments(
                self.attachment_busy,
                self.session_state == Some(SessionState::AwaitingInput),
            )
        {
            return;
        }
        self.attachment_busy = true;
        self.attachment_error = None;
        self.attachment_generation = self.attachment_generation.wrapping_add(1);
        let generation = self.attachment_generation;
        let link = self.link.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let uploaded = cx
                .background_spawn(async move {
                    let mut payloads = Vec::with_capacity(paths.len());
                    for path in paths {
                        let metadata = std::fs::metadata(&path)
                            .map_err(|error| format!("{}: {error}", path.display()))?;
                        if !metadata.is_file() {
                            return Err(format!("{}: not a file", path.display()));
                        }
                        let bytes = std::fs::read(&path)
                            .map_err(|error| format!("{}: {error}", path.display()))?;
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        payloads.push((name, bytes));
                    }
                    upload_attachment_payloads(link, payloads).await
                })
                .await;
            this.update(cx, |this, cx| {
                if this.attachment_generation != generation {
                    return;
                }
                this.attachment_busy = false;
                match uploaded {
                    Ok(uploaded) => this.attachments.extend(uploaded),
                    Err(error) => this.attachment_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Upload image bytes found on the system clipboard.
    ///
    /// This observes the same Paste action as the text area. Text-only paste
    /// remains entirely owned by the input, while image entries become
    /// ordinary daemon attachments.
    fn paste_attachments(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if !ginka_ui::composer::can_accept_attachments(
            self.attachment_busy,
            self.session_state == Some(SessionState::AwaitingInput),
        ) {
            return;
        }
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        let first_ordinal = self.attachments.len() + 1;
        let payloads = clipboard
            .entries
            .into_iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::Image(image) if !image.bytes.is_empty() => {
                    Some((image.format.extension().to_string(), image.bytes))
                }
                _ => None,
            })
            .enumerate()
            .map(|(index, (extension, bytes))| {
                (
                    ginka_ui::composer::pasted_image_name(first_ordinal + index, &extension),
                    bytes,
                )
            })
            .collect::<Vec<_>>();
        if payloads.is_empty() {
            return;
        }

        self.attachment_busy = true;
        self.attachment_error = None;
        self.attachment_generation = self.attachment_generation.wrapping_add(1);
        let generation = self.attachment_generation;
        let link = self.link.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let uploaded = cx
                .background_spawn(async move { upload_attachment_payloads(link, payloads).await })
                .await;
            this.update(cx, |this, cx| {
                if this.attachment_generation != generation {
                    return;
                }
                this.attachment_busy = false;
                match uploaded {
                    Ok(uploaded) => this.attachments.extend(uploaded),
                    Err(error) => this.attachment_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Remove one uploaded file from the next prompt.
    ///
    /// Its daemon blob is deliberately left for ordinary store cleanup: a
    /// client must not delete content another draft may already reference.
    fn remove_attachment(&mut self, reference: &str, cx: &mut Context<Self>) {
        self.attachments
            .retain(|attachment| attachment.attachment.reference != reference);
        self.attachment_error = None;
        cx.notify();
    }

    /// Open the non-destructive annotation dialog for an uploaded image.
    fn open_image_markup(&mut self, reference: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attachment) = self.attachments.iter().find(|attachment| {
            attachment.attachment.reference == reference && attachment.preview_url.is_some()
        }) else {
            return;
        };
        let text = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("composer.markup.text_placeholder").to_string())
        });
        self.image_markup = Some(ImageMarkup {
            source_reference: reference.to_string(),
            source_name: attachment.attachment.name.clone(),
            preview_url: attachment.preview_url.clone().unwrap_or_default(),
            document: ginka_ui::markup::MarkupDocument::default(),
            tool: ginka_ui::markup::MarkupTool::Pen,
            canvas_bounds: Bounds::default(),
            drawing: false,
            text,
        });
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.suspend_browser(true, cx));
        cx.notify();
    }

    fn markup_point(bounds: Bounds<Pixels>, position: Point<Pixels>) -> ginka_ui::markup::Point {
        ginka_ui::markup::Point::new(
            (position.x - bounds.origin.x)
                .as_f32()
                .clamp(0.0, bounds.size.width.as_f32()),
            (position.y - bounds.origin.y)
                .as_f32()
                .clamp(0.0, bounds.size.height.as_f32()),
        )
    }

    fn begin_markup(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(markup) = self.image_markup.as_mut() else {
            return;
        };
        let at = Self::markup_point(markup.canvas_bounds, event.position);
        if markup.tool == ginka_ui::markup::MarkupTool::Text {
            let text = markup.text.read(cx).value().to_string();
            markup.document.commit_text(at, text);
        } else {
            markup.document.begin(markup.tool, at);
            markup.drawing = true;
        }
        cx.notify();
    }

    fn extend_markup(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(markup) = self.image_markup.as_mut() else {
            return;
        };
        if !markup.drawing {
            return;
        }
        markup
            .document
            .extend(Self::markup_point(markup.canvas_bounds, event.position));
        cx.notify();
    }

    fn finish_markup(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(markup) = self.image_markup.as_mut() else {
            return;
        };
        if markup.drawing {
            markup
                .document
                .finish(Self::markup_point(markup.canvas_bounds, event.position));
            markup.drawing = false;
            cx.notify();
        }
    }

    /// Upload the flattened annotation through the same daemon path as every
    /// other attachment, replacing the unmarked source in this draft.
    fn submit_image_markup(&mut self, cx: &mut Context<Self>) {
        let Some(markup) = self.image_markup.as_ref() else {
            return;
        };
        let Some(svg) = ginka_ui::markup::compose_svg(
            &markup.preview_url,
            markup.canvas_bounds.size.width.as_f32(),
            markup.canvas_bounds.size.height.as_f32(),
            &markup.document,
        ) else {
            return;
        };
        let source_reference = markup.source_reference.clone();
        let name = format!("annotated-{}.svg", markup.source_name);
        let link = self.link.clone();
        let generation = self.attachment_generation;
        self.attachment_busy = true;
        self.attachment_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let uploaded = cx
                .background_spawn(async move {
                    upload_attachment_payloads(link, vec![(name, svg.into_bytes())]).await
                })
                .await;
            this.update(cx, |this, cx| {
                if this.attachment_generation != generation {
                    return;
                }
                this.attachment_busy = false;
                match uploaded {
                    Ok(uploaded) => {
                        this.attachments.retain(|attachment| {
                            attachment.attachment.reference != source_reference
                        });
                        this.attachments.extend(uploaded);
                        this.image_markup = None;
                        this.surfaces
                            .update(cx, |surfaces, cx| surfaces.suspend_browser(false, cx));
                    }
                    Err(error) => this.attachment_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Register the folder selected in the add-project dialog.
    fn submit_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.add_project.as_mut() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let draft = match validate_project_draft(
            dialog.name.read(cx).value().as_ref(),
            dialog.path.clone(),
        ) {
            Ok(draft) => draft,
            Err(ProjectDraftError::MissingName) => {
                dialog.error = Some(rust_i18n::t!("project.add.name.required").to_string());
                cx.notify();
                return;
            }
            Err(ProjectDraftError::MissingSource) => {
                dialog.error = Some(rust_i18n::t!("project.add.source.required").to_string());
                cx.notify();
                return;
            }
        };
        dialog.busy = true;
        dialog.error = None;
        cx.notify();

        let link = self.link.clone();
        cx.spawn_in(window, async move |this, cx| {
            let added = cx
                .background_spawn(
                    async move { link.add_project(draft.path, Some(draft.label)).await },
                )
                .await;
            this.update_in(cx, |this, window, cx| {
                if let Some(project) = added {
                    this.add_project = None;
                    this.start_new_chat(Some(project.name), window, cx);
                } else if let Some(dialog) = this.add_project.as_mut() {
                    dialog.busy = false;
                    dialog.error = Some(rust_i18n::t!("project.add.failed").to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Clear the column for a new conversation, aimed at `project`.
    ///
    /// A chat is started before it has a workspace: the reader picks a project
    /// (or none), says what they want, and only then is there a worktree and a
    /// session to show. Everything a conversation carries — its transcript,
    /// its checkpoints, its shells — belongs to the one being left, so all of
    /// it goes.
    fn start_new_chat(
        &mut self,
        project: Option<ProjectName>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigation
            .visit(NavigationTarget::Home(project.clone()));
        self.remember_workspace_view(cx);
        self.save_settings();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.aim_at(project.clone(), cx));
        self.target_project = project;
        self.session = None;
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.clear_file(cx));
        self.transcript = Transcript::new();
        self.transcript_of = None;
        self.transcript_search = None;
        self.session_state = None;
        self.submitted = false;
        self.transcript_follows = true;
        // The next prompt opens a session rather than continuing whichever one
        // the workspace it lands in happens to hold: that is what "new" said.
        self.start_fresh = true;
        self.picker = None;
        self.mentions.clear();
        self.commands.clear();
        self.queued_messages.clear();
        self.queue_can_send_now = false;
        self.editing_queued_message = None;
        self.queue_edit_draft = None;
        self.queue_error = None;
        self.attachments.clear();
        self.image_markup = None;
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.suspend_browser(false, cx));
        self.attachment_busy = false;
        self.attachment_error = None;
        self.attachment_generation = self.attachment_generation.wrapping_add(1);
        self.checkpoints = Vec::new();
        self.rewinding = None;
        self.forking = None;
        self.index_starting = false;
        self.index_error = None;
        // The shells belong to the workspace that is no longer on screen.
        self.terminal_search = None;
        self.terminals = ginka_ui::terminal::TerminalTabs::new();
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.composer.focus_handle(cx).focus(window, cx);
        if self.surfaces.read(cx).open_surface() == Some(ginka_ui::surface::Surface::Skills) {
            self.refresh_skills(cx);
        }
        cx.notify();
    }

    /// Show a workspace this window has just decided on.
    ///
    /// Before any refresh has listed it, which is why the row is passed in
    /// rather than looked up: a scratch worktree made half a second ago is
    /// where the prompt is about to go, and waiting a tick to admit it exists
    /// would leave the reader watching an empty window.
    fn adopt(&mut self, row: SessionRow, cx: &mut Context<Self>) {
        let changed_workspace =
            self.session.as_ref().map(|current| &current.workspace) != Some(&row.workspace);
        if changed_workspace {
            self.remember_workspace_view(cx);
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.adopt_workspace(row.workspace.clone(), cx)
        });
        self.target_project = Some(ProjectName(row.origin.to_string()));
        self.navigation
            .visit(NavigationTarget::Workspace(row.workspace.clone()));
        self.session = Some(row);
        if changed_workspace {
            let workspace = self
                .session
                .as_ref()
                .expect("a workspace was just adopted")
                .workspace
                .clone();
            self.restore_workspace_view(&workspace, cx);
            self.surfaces
                .update(cx, |surfaces, cx| surfaces.clear_file(cx));
            self.save_settings();
        }
        cx.notify();
    }

    /// The project the next prompt would run in, as the window says it.
    fn project_label(&self) -> Option<SharedString> {
        self.session
            .as_ref()
            .map(|row| row.origin.clone())
            .or_else(|| {
                self.target_project
                    .as_ref()
                    .map(|project| SharedString::from(project.0.clone()))
            })
    }

    /// Which agent the next prompt would start.
    ///
    /// A workspace decides for itself — it may already hold a conversation
    /// with one — and a chat that has no workspace yet takes what the reader
    /// picked, or the first agent on this machine that is actually usable.
    fn agent_to_start(&self) -> Option<String> {
        if let Some(row) = self.session.as_ref() {
            return Some(row.agent_to_start(&self.agents, self.chosen_agent.as_deref()));
        }
        if let Some(chosen) = self.chosen_agent.clone() {
            return Some(chosen);
        }
        self.agents
            .iter()
            .find(|agent| agent.is_ready())
            .or_else(|| self.agents.first())
            .map(|agent| agent.id.clone())
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
        if self.start_fresh {
            return;
        }
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
        self.scroll_to_search_hit();
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
        self.forking = None;
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

    /// Synchronize the selected branch through the daemon-owned git path.
    fn sync_git(&mut self, push: bool, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_spawn(async move {
                    if push {
                        link.push(&workspace).await
                    } else {
                        link.pull(&workspace).await
                    }
                })
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_git_sync_result(outcome.err(), cx)
            });
        })
        .detach();
    }

    /// Ask the daemon for a commit message. The answer comes back as an
    /// event, which is what keeps a model's thirty seconds off the request
    /// path; only a refusal to *start* is reported here.
    fn generate_commit_message(&mut self, only_staged: bool, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_spawn(async move {
                    link.generate_commit_message(&workspace, only_staged).await
                })
                .await;
            if let Err(error) = outcome {
                surfaces.update(cx, |surfaces, cx| {
                    surfaces.set_generated(None, Some(error), cx)
                });
            }
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

    /// Move one exact hunk across the index boundary and surface stale views.
    fn stage_hunk(&mut self, path: String, header: String, staged: bool, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    link.stage_hunk(&workspace, &path, &header, staged).await
                })
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_git_sync_result(outcome.err(), cx)
            });
            this.update(cx, |this, cx| this.refresh_changes(cx)).ok();
        })
        .detach();
    }

    /// Permanently discard one exact unstaged hunk and surface stale views.
    fn revert_hunk(&mut self, path: String, header: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move { link.revert_hunk(&workspace, &path, &header).await })
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_git_sync_result(outcome.err(), cx)
            });
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
            let (changes, staged_changes, comments) = cx
                .background_spawn(async move {
                    let changes = link
                        .changes(&workspace, ginka_protocol::ChangeSource::Unstaged)
                        .await;
                    let staged_changes = link
                        .changes(&workspace, ginka_protocol::ChangeSource::Staged)
                        .await;
                    let comments = link.comments(&workspace).await;
                    (changes, staged_changes, comments)
                })
                .await;
            surfaces.update(cx, |surfaces, cx| {
                surfaces.set_changes(changes, cx);
                let staged = staged_changes
                    .as_ref()
                    .map(|changes| changes.files.iter().map(|file| file.path.clone()).collect())
                    .unwrap_or_default();
                surfaces.set_staged_changes(staged_changes, cx);
                surfaces.set_staged(staged, cx);
                surfaces.set_comments(comments, cx);
            });
        })
        .detach();
    }

    /// Refresh bounded history only while the reader has expanded it.
    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        cx.spawn(async move |_, cx| {
            let history = cx
                .background_spawn(async move { link.history(&workspace, 50).await })
                .await;
            surfaces.update(cx, |surfaces, cx| surfaces.set_history(history, cx));
        })
        .detach();
    }

    /// Find the workspace's files that match what was typed into the finder.
    fn find_files(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let scope = self.surfaces.read(cx).file_search_scope();
        let Some(project) = self.target_project.clone() else {
            return;
        };
        let target = scope.target(workspace.clone(), project);
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        if query.trim().is_empty() {
            surfaces.update(cx, |surfaces, cx| {
                surfaces.begin_file_tree(workspace.clone(), cx)
            });
            cx.spawn(async move |_, cx| {
                let requested_workspace = workspace.clone();
                let (files, truncated) = cx
                    .background_spawn(async move { link.file_tree(&workspace).await })
                    .await;
                surfaces.update(cx, |surfaces, cx| {
                    surfaces.set_file_tree(requested_workspace, files, truncated, cx);
                });
            })
            .detach();
            return;
        }
        if let ginka_ui::file_search::FileSearchTarget::Project(project) = target {
            surfaces.update(cx, |surfaces, cx| {
                surfaces.begin_project_search(project.clone(), query.clone(), cx)
            });
            cx.spawn(async move |_, cx| {
                let requested_project = project.clone();
                let requested_query = query.clone();
                let (files, matches) = cx
                    .background_spawn(async move { link.search_project(&project, &query).await })
                    .await;
                surfaces.update(cx, |surfaces, cx| {
                    surfaces.set_project_matches(
                        requested_project,
                        requested_query,
                        files,
                        matches,
                        cx,
                    );
                });
            })
            .detach();
            return;
        }
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

    /// Open a project-search hit, switching the centre column when necessary.
    fn open_workspace_file(
        &mut self,
        workspace: WorkspaceId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.as_ref().map(|row| &row.workspace) == Some(&workspace) {
            self.open_file(path, false, window, cx);
            return;
        }
        self.pending_file_open = Some((workspace.clone(), path));
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_workspace(&workspace, cx));
    }

    /// Read a file and show it in the files surface.
    fn open_file(
        &mut self,
        path: String,
        from_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let worktree = self
            .session
            .as_ref()
            .map(|row| row.path.clone())
            .expect("the selected session has a worktree");
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        let should_read = surfaces.update(cx, |surfaces, cx| {
            surfaces.begin_file_open(workspace.clone(), path.clone(), from_history, cx)
        });
        if !should_read {
            return;
        }
        let read_workspace = workspace.clone();
        let read_path = path.clone();
        cx.spawn_in(window, async move |_, cx| {
            let file = cx
                .background_spawn(async move { link.read_file(&read_workspace, &read_path).await })
                .await;
            let _ = surfaces.update_in(cx, |surfaces, window, cx| {
                surfaces.set_file(workspace, worktree, path, file, window, cx)
            });
        })
        .detach();
    }

    /// Open a language-server definition in this workspace and select it.
    fn open_definition(
        &mut self,
        source_workspace: &WorkspaceId,
        target: ginka_ui::editor::DefinitionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        if &workspace != source_workspace {
            return;
        }
        let worktree = self
            .session
            .as_ref()
            .map(|row| row.path.clone())
            .expect("the selected session has a worktree");
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        let path = target.path.clone();
        let should_read = surfaces.update(cx, |surfaces, cx| {
            surfaces.begin_definition_open(workspace.clone(), target, window, cx)
        });
        if !should_read {
            return;
        }
        let read_workspace = workspace.clone();
        let read_path = path.clone();
        cx.spawn_in(window, async move |_, cx| {
            let file = cx
                .background_spawn(async move { link.read_file(&read_workspace, &read_path).await })
                .await;
            let _ = surfaces.update_in(cx, |surfaces, window, cx| {
                surfaces.set_file(workspace, worktree, path, file, window, cx)
            });
        })
        .detach();
    }

    /// Save the current editor buffer without overwriting a concurrent edit.
    fn save_file(
        &mut self,
        workspace: WorkspaceId,
        path: String,
        text: String,
        expected_revision: String,
        cx: &mut Context<Self>,
    ) {
        let link = self.link.clone();
        let surfaces = self.surfaces.clone();
        let saved_workspace = workspace.clone();
        let saved_path = path.clone();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_spawn(async move {
                    link.write_file(&workspace, &path, text, expected_revision)
                        .await
                })
                .await;
            surfaces.update(cx, |surfaces, cx| match result {
                Ok(file) => surfaces.set_file_saved(&saved_workspace, file, cx),
                Err(error) => {
                    surfaces.set_file_save_error(&saved_workspace, &saved_path, error, cx)
                }
            });
        })
        .detach();
    }

    /// Add an editor selection's source location to the current chat draft.
    fn add_file_reference(&mut self, reference: &str, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value();
        let draft = ginka_ui::editor::append_reference(&draft, reference);
        self.composer
            .update(cx, |state, cx| state.set_value(draft, window, cx));
        self.composer.focus_handle(cx).focus(window, cx);
    }

    /// Add a sanitized inspect-mode bundle to the current chat draft.
    fn add_browser_context(&mut self, context: &str, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value();
        let draft = ginka_ui::browser::append_context(&draft, context);
        self.composer
            .update(cx, |state, cx| state.set_value(draft, window, cx));
        self.composer.focus_handle(cx).focus(window, cx);
    }

    /// Append a transcript message to the draft as a Markdown quote.
    fn quote_in_composer(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value();
        let draft = ginka_ui::composer::append_quote(&draft, message);
        self.composer
            .update(cx, |state, cx| state.set_value(draft, window, cx));
        self.composer.focus_handle(cx).focus(window, cx);
    }

    /// Actions shared by the readable messages in a transcript.
    fn message_actions(
        &self,
        index: usize,
        role: &'static str,
        message: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let copied = message.to_string();
        let quoted = message.to_string();
        h_flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("copy-{role}-{index}")))
                    .ghost()
                    .compact()
                    .tooltip(rust_i18n::t!("transcript.copy.tooltip").to_string())
                    .child(rust_i18n::t!("transcript.copy").to_string())
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("quote-{role}-{index}")))
                    .ghost()
                    .compact()
                    .tooltip(rust_i18n::t!("transcript.quote.tooltip").to_string())
                    .child(rust_i18n::t!("transcript.quote").to_string())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let selection = TextSelection::selected_text(window, cx);
                        let target = ginka_ui::transcript::quote_target(&quoted, &selection);
                        this.quote_in_composer(target, window, cx)
                    })),
            )
            .into_any_element()
    }

    /// Start a shell in the workspace on screen.
    ///
    /// Sized for the dock as it is now, and focused, because someone who
    /// opened a terminal means to type in it.
    fn open_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_terminal_with_input(None, false, window, cx);
    }

    /// Start a shell and optionally paste initial input once the daemon owns it.
    fn open_terminal_with_input(
        &mut self,
        input: Option<String>,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let (rows, mut cols) = self.dock_size();
        if split && self.terminals.split_ids().is_none() {
            cols = (cols / 2).max(1);
        } else if !split && self.terminals.split_ids().is_some() {
            cols = TERMINAL_COLUMNS;
        }
        let link = self.link.clone();
        self.terminal_focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move {
                    let terminal = link.open_terminal(&workspace, rows, cols).await;
                    if let (Some(terminal), Some(input)) = (&terminal, input) {
                        link.write_terminal(terminal, input).await;
                    }
                    terminal
                })
                .await;
            if let Some(terminal) = opened {
                this.update(cx, |this, cx| {
                    let title = this.terminals.tabs().len() + 1;
                    this.terminal_search = None;
                    if split {
                        this.terminals
                            .open_split(terminal, format!("shell {title}"), rows, cols);
                        this.resize_terminal(cx);
                    } else {
                        this.terminals
                            .open(terminal, format!("shell {title}"), rows, cols);
                        this.resize_terminal(cx);
                    }
                    this.persist();
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

    /// Add a daemon terminal beside the active pane, or collapse the split.
    fn toggle_terminal_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.split_ids().is_some() {
            self.terminals.unsplit();
            self.resize_terminal(cx);
            self.persist();
            self.terminal_focus.focus(window, cx);
            cx.notify();
        } else {
            self.open_terminal_with_input(None, true, window, cx);
        }
    }

    /// Paste an editor selection into the terminal without appending Return.
    fn write_terminal_selection(
        &mut self,
        selection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.layout.is_open(Panel::TerminalDock) {
            self.toggle(Panel::TerminalDock, cx);
        }
        self.terminal_focus.focus(window, cx);
        let Some(terminal) = self.terminals.active_id() else {
            self.open_terminal_with_input(Some(selection), false, window, cx);
            return;
        };
        let link = self.link.clone();
        cx.background_spawn(async move { link.write_terminal(&terminal, selection).await })
            .detach();
    }

    /// Start or refresh semantic indexing in a visible daemon terminal.
    fn index_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.index_starting {
            return;
        }
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        if !self.layout.is_open(Panel::TerminalDock) {
            self.toggle(Panel::TerminalDock, cx);
        }
        let (rows, cols) = self.dock_size();
        self.index_starting = true;
        self.index_error = None;
        self.terminal_focus.focus(window, cx);
        cx.notify();

        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.index_workspace(&workspace, rows, cols).await })
                .await;
            this.update(cx, |this, cx| {
                this.index_starting = false;
                match result {
                    Ok(terminal) => {
                        let title = this.terminals.tabs().len() + 1;
                        this.terminal_search = None;
                        this.terminals
                            .open(terminal, format!("shell {title}"), rows, cols);
                        this.resize_terminal(cx);
                        this.persist();
                        this.adopt_terminals(cx);
                    }
                    Err(error) => this.index_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the selected project's skills plus the user's own from the daemon.
    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.begin_skill_refresh(cx));
        let link = self.link.clone();
        let project = self.target_project.clone();
        let expected_project = project.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.skills(project).await })
                .await;
            this.update(cx, |this, cx| {
                if this.target_project == expected_project {
                    this.surfaces
                        .update(cx, |surfaces, cx| surfaces.set_skills(result, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Enable or disable every installed copy of one grouped skill.
    fn set_skill_enabled(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        self.surfaces.update(cx, |surfaces, cx| {
            surfaces.begin_skill_change(name.clone(), cx)
        });
        let link = self.link.clone();
        let mutation_project = self.target_project.clone();
        let listing_project = mutation_project.clone();
        let expected_project = mutation_project.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    link.set_skill_enabled(name, enabled, mutation_project)
                        .await?;
                    link.skills(listing_project).await
                })
                .await;
            this.update(cx, |this, cx| {
                if this.target_project == expected_project {
                    this.surfaces
                        .update(cx, |surfaces, cx| surfaces.set_skills(result, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Run the vendor's sign-in for a login in the dock, pointed at the
    /// login's directory (`docs/accounts.md` §4).
    ///
    /// The daemon owns the terminal and re-probes the login when the command
    /// exits, so the chip changes on its own. Needs a workspace for the dock
    /// to belong to; on the home screen there is none, and the CLI's
    /// `ginka account login` is the way.
    fn sign_in(
        &mut self,
        account: ginka_protocol::AccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let (rows, cols) = self.dock_size();
        let link = self.link.clone();
        self.terminal_focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move {
                    link.login_account(&account, &workspace, rows, cols).await
                })
                .await;
            if let Some(terminal) = opened {
                this.update(cx, |this, cx| {
                    let title = this.terminals.tabs().len() + 1;
                    this.terminal_search = None;
                    this.terminals
                        .open(terminal, format!("shell {title}"), rows, cols);
                    this.resize_terminal(cx);
                    this.persist();
                    // Adopted straight afterwards so the tab takes the name
                    // the daemon gave it, which says whose sign-in it is.
                    this.adopt_terminals(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Confirm, then stop one of the dock's running shells.
    fn request_terminal_close(
        &mut self,
        terminal: ginka_protocol::TerminalId,
        cx: &mut Context<Self>,
    ) {
        let was_split = self.terminals.split_ids().is_some();
        match self.terminals.request_close(&terminal) {
            ginka_ui::terminal::CloseRequest::Confirm => cx.notify(),
            ginka_ui::terminal::CloseRequest::Close => {
                if self
                    .terminal_search
                    .as_ref()
                    .is_some_and(|search| search.terminal == terminal)
                {
                    self.terminal_search = None;
                }
                if was_split && self.terminals.split_ids().is_none() {
                    self.resize_terminal(cx);
                }
                self.persist();
                cx.notify();
                let link = self.link.clone();
                cx.background_spawn(async move { link.close_terminal(&terminal).await })
                    .detach();
            }
            ginka_ui::terminal::CloseRequest::Missing => {}
        }
    }

    /// Bring one of the dock's shells to the front.
    fn show_terminal(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let was_split = self.terminals.split_ids().is_some();
        self.terminal_search = None;
        self.terminals.focus(index);
        if was_split && self.terminals.split_ids().is_none() {
            self.resize_terminal(cx);
        }
        self.persist();
        self.terminal_focus.focus(window, cx);
        cx.notify();
    }

    /// Focus one pane of a split without collapsing the pair.
    fn focus_terminal(
        &mut self,
        terminal: &ginka_protocol::TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.terminals.active_id().as_ref() != Some(terminal);
        if changed {
            self.terminal_search = None;
            self.terminals.focus_id(terminal);
            self.persist();
        }
        self.terminal_focus.focus(window, cx);
        if changed {
            cx.notify();
        }
    }

    /// Move keyboard input to the other visible terminal pane.
    fn focus_other_terminal_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.focus_other_pane() {
            self.terminal_search = None;
            self.persist();
            self.terminal_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Find the shells the daemon kept running in this workspace, and replay
    /// what they printed while this window was not looking.
    fn adopt_terminals(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        let (rows, cols) = self.dock_size();
        let link = self.link.clone();
        let requested_workspace = workspace.clone();
        cx.spawn(async move |this, cx| {
            let running = cx
                .background_spawn(async move { link.terminals(&requested_workspace).await })
                .await;
            let fresh = this
                .update(cx, |this, cx| {
                    if this.session.as_ref().map(|row| &row.workspace) != Some(&workspace) {
                        return Vec::new();
                    }
                    let was_split = this.terminals.split_ids().is_some();
                    let fresh = this.terminals.adopt(&running, rows, cols);
                    let remembered = this
                        .settings
                        .workspace_layouts
                        .get(&workspace.0)
                        .map(|saved| (saved.terminal_split.clone(), saved.terminal_active.clone()))
                        .unwrap_or_default();
                    let active = remembered
                        .1
                        .as_ref()
                        .map(|id| ginka_protocol::TerminalId(id.clone()));
                    if let Some(split) = remembered.0.map(|ids| ids.map(ginka_protocol::TerminalId))
                    {
                        if !this.terminals.restore_split(split, active.as_ref())
                            && let Some(active) = active.as_ref()
                        {
                            this.terminals.focus_id(active);
                        }
                    } else if let Some(active) = active.as_ref() {
                        this.terminals.focus_id(active);
                    }
                    if (was_split && this.terminals.split_ids().is_none())
                        || (!was_split && this.terminals.split_ids().is_some())
                    {
                        this.resize_terminal(cx);
                    }
                    cx.notify();
                    fresh
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
        let columns = if self.terminals.split_ids().is_some() {
            (TERMINAL_COLUMNS / 2).max(1)
        } else {
            TERMINAL_COLUMNS
        };
        (rows, columns)
    }

    /// Send a keystroke to the shell.
    ///
    /// Translated here rather than passed as a key name, because a pty takes
    /// bytes: what a terminal *is* is a program reading the bytes a keyboard
    /// produced.
    fn type_into_terminal(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        self.terminal_selection = None;
        if event.keystroke.modifiers.shift
            && matches!(event.keystroke.key.as_str(), "pageup" | "pagedown")
        {
            let direction = if event.keystroke.key == "pageup" {
                1
            } else {
                -1
            };
            if let Some(screen) = self.terminals.active_mut().map(|tab| &mut tab.screen) {
                screen.scroll(direction * i32::from(screen.rows().saturating_sub(1).max(1)));
                cx.notify();
            }
            return;
        }
        let Some((terminal, data)) = self.terminals.active().and_then(|tab| {
            tab.screen
                .key_input(&event.keystroke)
                .map(|data| (tab.id.clone(), data))
        }) else {
            return;
        };
        let link = self.link.clone();
        cx.background_spawn(async move { link.write_terminal(&terminal, data).await })
            .detach();
    }

    /// Paste clipboard text using the active terminal's negotiated paste mode.
    fn paste_into_terminal(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|clipboard| clipboard.text())
        else {
            return;
        };
        let Some((terminal, data)) = self
            .terminals
            .active()
            .map(|tab| (tab.id.clone(), tab.screen.paste_input(&text)))
        else {
            return;
        };
        if data.is_empty() {
            return;
        }
        self.terminal_selection = None;
        cx.stop_propagation();
        let link = self.link.clone();
        cx.background_spawn(async move { link.write_terminal(&terminal, data).await })
            .detach();
    }

    fn active_terminal_selection_text(&self) -> Option<String> {
        self.terminals.active().and_then(|tab| {
            self.terminal_selection
                .as_ref()
                .filter(|drag| drag.terminal == tab.id)
                .and_then(|drag| tab.screen.selection_text(drag.selection))
        })
    }

    fn quoteable_terminal_selection(&self) -> Option<String> {
        self.active_terminal_selection_text()
            .filter(|selection| !selection.trim().is_empty())
    }

    /// Copy the active selection, or the viewport when nothing is selected.
    fn copy_terminal_output(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.active_terminal_selection_text().or_else(|| {
            self.terminals
                .active()
                .map(|tab| tab.screen.text())
                .filter(|text| !text.is_empty())
        }) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Append the active terminal selection to the composer as a quote.
    fn quote_terminal_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selection) = self.quoteable_terminal_selection() {
            self.quote_in_composer(&selection, window, cx);
        }
    }

    /// Translate a pointer position into the fixed terminal grid.
    fn terminal_point(
        &self,
        terminal: &ginka_protocol::TerminalId,
        position: Point<Pixels>,
    ) -> Option<ginka_ui::terminal::TerminalPoint> {
        let bounds = self.terminal_bounds.get(terminal)?;
        let tab = self
            .terminals
            .tabs()
            .iter()
            .find(|tab| &tab.id == terminal)?;
        let x = (position.x - bounds.origin.x - px(8.)).max(px(0.));
        let y = (position.y - bounds.origin.y - px(4.)).max(px(0.));
        let column = (x.as_f32() / TERMINAL_CELL_WIDTH).floor() as usize;
        let row = (y.as_f32() / TERMINAL_LINE_HEIGHT).floor() as usize;
        Some(ginka_ui::terminal::TerminalPoint::new(
            row.min(tab.screen.rows() as usize - 1),
            column.min(tab.screen.cols() as usize - 1),
        ))
    }

    fn begin_terminal_selection(
        &mut self,
        terminal: &ginka_protocol::TerminalId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_terminal(terminal, window, cx);
        if let Some(point) = self.terminal_point(terminal, position) {
            self.terminal_selection = Some(TerminalDrag {
                terminal: terminal.clone(),
                selection: ginka_ui::terminal::TerminalSelection::new(point, point),
            });
            cx.notify();
        }
    }

    fn extend_terminal_selection(
        &mut self,
        terminal: &ginka_protocol::TerminalId,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(point) = self.terminal_point(terminal, event.position) else {
            return;
        };
        if let Some(drag) = self
            .terminal_selection
            .as_mut()
            .filter(|drag| &drag.terminal == terminal)
            && drag.selection.head != point
        {
            drag.selection.head = point;
            cx.notify();
        }
    }

    fn on_copy_terminal_output(
        &mut self,
        _: &CopyTerminalOutput,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.copy_terminal_output(cx);
    }

    /// Browse terminal history without sending wheel movement to the pty.
    fn scroll_terminal(
        &mut self,
        terminal: &ginka_protocol::TerminalId,
        event: &ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) {
        let pixels = f32::from(event.delta.pixel_delta(px(17.)).y);
        if pixels == 0.0 {
            return;
        }
        self.terminal_selection = None;
        cx.stop_propagation();
        let lines = (pixels.abs() / 17.0).ceil() as i32 * if pixels > 0.0 { 1 } else { -1 };
        if let Some(screen) = self
            .terminals
            .tabs_mut()
            .iter_mut()
            .find(|tab| &tab.id == terminal)
            .map(|tab| &mut tab.screen)
        {
            let before = screen.display_offset();
            screen.scroll(lines);
            if screen.display_offset() != before {
                cx.notify();
            }
        }
    }

    /// Jump from terminal history to the newest output.
    fn terminal_to_live(&mut self, cx: &mut Context<Self>) {
        if let Some(screen) = self.terminals.active_mut().map(|tab| &mut tab.screen) {
            screen.scroll_to_live();
            cx.notify();
        }
    }

    /// Open find-in-terminal for the shell in front.
    fn open_terminal_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.active_id() else {
            return;
        };
        if let Some(search) = self
            .terminal_search
            .as_ref()
            .filter(|search| search.terminal == terminal)
        {
            search.query.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("terminal.search.placeholder").to_string())
        });
        cx.subscribe(&query, |this, query, event: &InputEvent, cx| match event {
            InputEvent::Change => this.search_terminal(query.read(cx).value().to_string(), cx),
            InputEvent::PressEnter { shift, .. } => this.step_terminal_search(
                if *shift {
                    ginka_ui::search::Direction::Previous
                } else {
                    ginka_ui::search::Direction::Next
                },
                cx,
            ),
            _ => {}
        })
        .detach();
        query.read(cx).focus_handle(cx).focus(window, cx);
        self.terminal_search = Some(TerminalSearch {
            terminal,
            query,
            matches: Vec::new(),
            chosen: None,
        });
        cx.notify();
    }

    /// Recompute matches after the terminal query changes.
    fn search_terminal(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(terminal) = self
            .terminal_search
            .as_ref()
            .map(|search| search.terminal.clone())
        else {
            return;
        };
        let matches = self
            .terminals
            .tabs()
            .iter()
            .find(|tab| tab.id == terminal)
            .map(|tab| tab.screen.refresh_search(&query, None))
            .unwrap_or_default();
        let search = self
            .terminal_search
            .as_mut()
            .expect("the terminal search was read above");
        (search.matches, search.chosen) = matches;
        self.reveal_terminal_search();
        cx.notify();
    }

    /// Recompute an open terminal search after that shell prints more output.
    fn refresh_terminal_search(
        &mut self,
        terminal: &ginka_protocol::TerminalId,
        cx: &mut Context<Self>,
    ) {
        let Some((query, chosen)) = self
            .terminal_search
            .as_ref()
            .filter(|search| &search.terminal == terminal)
            .map(|search| (search.query.read(cx).value().to_string(), search.chosen))
        else {
            return;
        };
        let Some(results) = self
            .terminals
            .tabs()
            .iter()
            .find(|tab| &tab.id == terminal)
            .map(|tab| tab.screen.refresh_search(&query, chosen))
        else {
            return;
        };
        if let Some(search) = self.terminal_search.as_mut() {
            (search.matches, search.chosen) = results;
        }
    }

    /// Move to another terminal match, wrapping at either end.
    fn step_terminal_search(
        &mut self,
        direction: ginka_ui::search::Direction,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.terminal_search.as_mut() else {
            return;
        };
        search.chosen = ginka_ui::search::step(search.matches.len(), search.chosen, direction);
        self.reveal_terminal_search();
        cx.notify();
    }

    /// Put the selected terminal match inside the viewport.
    fn reveal_terminal_search(&mut self) {
        let Some((terminal, found)) = self.terminal_search.as_ref().and_then(|search| {
            search
                .chosen
                .and_then(|chosen| search.matches.get(chosen))
                .map(|found| (search.terminal.clone(), found.clone()))
        }) else {
            return;
        };
        if let Some(tab) = self
            .terminals
            .tabs_mut()
            .iter_mut()
            .find(|tab| tab.id == terminal)
        {
            tab.screen.reveal_search_match(&found);
        }
    }

    /// Close terminal find and return keyboard input to the shell.
    fn close_terminal_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_search = None;
        self.terminal_focus.focus(window, cx);
        cx.notify();
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

    /// Re-read the selected conversation's daemon-owned follow-up queue.
    fn refresh_queue(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            self.queued_messages.clear();
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let _ = pull_queue(&this, &link, session, cx).await;
        })
        .detach();
    }

    /// Put one queued prompt in the composer without losing its normal draft.
    fn edit_queued_message(
        &mut self,
        id: u64,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_queued_message.is_none() {
            self.queue_edit_draft = Some(self.composer.read(cx).value().to_string());
        }
        self.editing_queued_message = Some(id);
        self.queue_error = None;
        self.composer
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.composer.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Leave queue editing and restore the draft it displaced.
    fn cancel_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.queue_edit_draft.take().unwrap_or_default();
        self.editing_queued_message = None;
        self.queue_error = None;
        self.composer
            .update(cx, |state, cx| state.set_value(draft, window, cx));
        cx.notify();
    }

    /// Persist the queued prompt currently being edited.
    fn save_queued_edit(
        &mut self,
        session: SessionId,
        id: u64,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let link = self.link.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.edit_queued_message(&session, id, text).await })
                .await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        let draft = this.queue_edit_draft.take().unwrap_or_default();
                        this.editing_queued_message = None;
                        this.queue_error = None;
                        this.composer
                            .update(cx, |state, cx| state.set_value(draft, window, cx));
                    }
                    Err(error) => this.queue_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Remove one queued prompt.
    fn remove_queued_message(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.remove_queued_message(&session, id).await })
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.queue_error = Some(error);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Move one queued prompt to a zero-based dispatch position.
    fn move_queued_message(&mut self, id: u64, index: u32, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { link.move_queued_message(&session, id, index).await },
                )
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.queue_error = Some(error);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ask the live transport to take one waiting prompt immediately.
    fn send_queued_message_now(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            return;
        };
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.send_queued_message_now(&session, id).await })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.queue_error = None,
                    Err(error) => this.queue_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Send what is in the composer.
    ///
    /// A chat that has no workspace yet — the home screen — gets one first,
    /// which is either the chosen project's checkout or a scratch worktree.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.attachment_busy {
            return;
        }
        let draft = self.composer.read(cx).value().trim().to_string();
        if let Some(id) = self.editing_queued_message
            && let Some(session) = self.session.as_ref().and_then(|row| row.session.clone())
        {
            self.save_queued_edit(session, id, draft, window, cx);
            return;
        }
        if self.session_state == Some(SessionState::AwaitingInput)
            && let Some(session) = self.session.as_ref().and_then(|row| row.session.clone())
            && let Some(request_id) = self.transcript.open_request().map(str::to_string)
        {
            if draft.is_empty() {
                return;
            }
            self.respond_from_composer(session, request_id, draft, window, cx);
            return;
        }
        let references = self
            .attachments
            .iter()
            .map(|attachment| attachment.attachment.reference.as_str())
            .collect::<Vec<_>>();
        let Some(text) = ginka_ui::composer::submission(&draft, &references) else {
            return;
        };
        match self.session.clone() {
            Some(row) => self.send(row, text, window, cx),
            None => self.send_first(text, window, cx),
        }
    }

    /// Give a chat that has no workspace yet somewhere to run, then send.
    ///
    /// With a project chosen the chat runs in its checkout: a session there is
    /// what "a new chat in this project" is. With none it runs in a scratch
    /// worktree the daemon makes — a question that needs somewhere to work
    /// should not need a repository first.
    ///
    /// The composer is not cleared here. Nothing has been sent until there is
    /// a workspace, and a prompt cleared out of a box that then failed to go
    /// anywhere is a prompt the reader has to retype.
    fn send_first(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.target_project.clone() {
            let Some(project) = self
                .projects
                .iter()
                .find(|project| project.name == name)
                .cloned()
            else {
                tracing::warn!(project = %name.0, "the chosen project is no longer registered");
                return;
            };
            let rows = self.sidebar.read(cx).rows().to_vec();
            let landed = workspace_for_new_chat(&rows, &project)
                .and_then(|workspace| rows.into_iter().find(|row| row.workspace == workspace));
            let Some(row) = landed else {
                // A project with no worktree at all. Cutting a branch behind a
                // prompt is not what the reader asked for, so the composer
                // keeps what they typed and the chip says why nothing moved.
                tracing::warn!(project = %name.0, "no workspace to start a chat in");
                return;
            };
            self.adopt(row.clone(), cx);
            self.send(row, text, window, cx);
            return;
        }

        // Somewhere to work is made for it. Said before the daemon answers,
        // because making a worktree is a round trip and a window that says
        // nothing for one reads as a window that dropped the prompt.
        self.submitted = true;
        self.transcript_follows = true;
        cx.notify();
        let link = self.link.clone();
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_spawn(async move { link.create_scratch().await })
                .await;
            let Some(summary) = made else {
                // Nothing was made, so nothing was sent: stop claiming to be
                // working and leave the prompt where the reader can send it
                // again.
                this.update(cx, |this, cx| {
                    this.submitted = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            let row = SessionRow::from_summary(&summary, crate::daemon::now());
            this.update_in(cx, |this, window, cx| {
                this.adopt(row.clone(), cx);
                this.send(row, text, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Send `text` into a workspace.
    ///
    /// With a session already in it this is a follow-up, queued by the daemon
    /// if the agent is mid-turn. Without one — or after "new chat" — it starts
    /// an agent. Either way the box is cleared straight away: the prompt is
    /// the daemon's now, and leaving it behind invites sending it twice.
    fn send(&mut self, row: SessionRow, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.attachments.clear();
        self.image_markup = None;
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.suspend_browser(false, cx));
        self.attachment_error = None;
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
        let agent = self
            .agent_to_start()
            .unwrap_or_else(|| row.agent_to_start(&self.agents, self.chosen_agent.as_deref()));
        let model = self.model_to_start();
        let (reasoning_effort, service_tier) = self.model_options_to_start();
        let access = self.chosen_access;
        let account = self.account_to_start().map(|account| account.id.clone());
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
                            link.start_session(SessionLaunch {
                                workspace,
                                agent,
                                prompt: text,
                                model,
                                reasoning_effort,
                                service_tier,
                                access,
                                account,
                            })
                            .await
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
                                this.transcript_search = None;
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
        if let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) {
            self.layout
                .write_workspace_into(&workspace, &mut self.settings);
            self.write_terminal_arrangement(&workspace);
        } else {
            self.layout.write_into(&mut self.settings);
        }
        self.save_settings();
    }

    fn save_settings(&self) {
        if let Err(error) = settings::save(&self.paths.app_settings(), &self.settings) {
            tracing::warn!(%error, "could not persist the panel layout");
        }
    }

    /// Capture the workspace-owned panels and selected surface before leaving.
    fn remember_workspace_view(&mut self, cx: &App) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        self.layout
            .write_workspace_into(&workspace, &mut self.settings);
        if let Some(saved) = self.settings.workspace_layouts.get_mut(&workspace.0) {
            saved.active_surface = self
                .surfaces
                .read(cx)
                .open_surface()
                .map(|surface| surface.key().to_string());
        }
        self.write_terminal_arrangement(&workspace);
    }

    /// Store terminal ids as restorable view state, never as process ownership.
    fn write_terminal_arrangement(&mut self, workspace: &WorkspaceId) {
        let split = self.terminals.split_ids().map(|ids| ids.map(|id| id.0));
        let active = self.terminals.active_id().map(|id| id.0);
        if let Some(saved) = self.settings.workspace_layouts.get_mut(&workspace.0) {
            saved.terminal_split = split;
            saved.terminal_active = active;
        }
    }

    /// Restore the panels and selected surface belonging to the new workspace.
    fn restore_workspace_view(&mut self, workspace: &WorkspaceId, cx: &mut Context<Self>) {
        self.layout = Layout::for_workspace(&self.settings, Some(workspace));
        let surface = self
            .settings
            .workspace_layouts
            .get(&workspace.0)
            .and_then(|saved| saved.active_surface.as_deref())
            .and_then(ginka_ui::surface::Surface::from_key);
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.restore_surface(surface, cx));
    }

    fn persist_surface(&mut self, surface: Option<ginka_ui::surface::Surface>) {
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        self.layout
            .write_workspace_into(&workspace, &mut self.settings);
        if let Some(saved) = self.settings.workspace_layouts.get_mut(&workspace.0) {
            saved.active_surface = surface.map(|surface| surface.key().to_string());
        }
        self.save_settings();
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

    fn cycle_surface(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.layout.is_open(Panel::RightPanel) {
            self.toggle(Panel::RightPanel, cx);
        }
        let current = self.surfaces.read(cx).open_surface();
        let surface = if forward {
            ginka_ui::surface::Surface::next(current)
        } else {
            ginka_ui::surface::Surface::previous(current)
        };
        self.surfaces
            .update(cx, |surfaces, cx| surfaces.show(surface, cx));
    }

    fn on_next_surface(&mut self, _: &NextSurface, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_surface(true, cx);
    }

    fn on_previous_surface(&mut self, _: &PreviousSurface, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_surface(false, cx);
    }

    fn cycle_terminal_tab(&mut self, previous: bool, cx: &mut Context<Self>) {
        let moved = if previous {
            self.terminals.focus_previous()
        } else {
            self.terminals.focus_next()
        };
        if moved {
            self.terminal_search = None;
            self.persist();
            cx.notify();
        }
    }

    fn on_next_terminal_tab(
        &mut self,
        _: &NextTerminalTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_terminal_tab(false, cx);
    }

    fn on_previous_terminal_tab(
        &mut self,
        _: &PreviousTerminalTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_terminal_tab(true, cx);
    }

    /// Whether a recorded destination still exists in the daemon's latest list.
    fn navigation_target_available(
        target: &NavigationTarget,
        projects: &[ProjectRow],
        rows: &[SessionRow],
    ) -> bool {
        match target {
            NavigationTarget::Home(None) => true,
            NavigationTarget::Home(Some(project)) => {
                projects.iter().any(|candidate| &candidate.name == project)
            }
            NavigationTarget::Workspace(workspace) => rows
                .iter()
                .any(|row| &row.workspace == workspace && !row.archived),
        }
    }

    /// Whether the two title-bar history directions can reach a live target.
    fn navigation_capabilities(&self, cx: &App) -> (bool, bool) {
        let rows = self.sidebar.read(cx).rows();
        let available = |target: &NavigationTarget| {
            Self::navigation_target_available(target, &self.projects, rows)
        };
        (
            self.navigation.can_go_back_where(available),
            self.navigation.can_go_forward_where(available),
        )
    }

    /// Move through project/session visits, skipping removed or archived workspaces.
    fn navigate_history(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        let projects = self.projects.clone();
        let rows = self.sidebar.read(cx).rows().to_vec();
        let available =
            |target: &NavigationTarget| Self::navigation_target_available(target, &projects, &rows);
        let target = if backwards {
            self.navigation.back_where(available)
        } else {
            self.navigation.forward_where(available)
        };
        match target {
            Some(NavigationTarget::Home(project)) => self.start_new_chat(project, window, cx),
            Some(NavigationTarget::Workspace(workspace)) => self
                .sidebar
                .update(cx, |sidebar, cx| sidebar.select_workspace(&workspace, cx)),
            None => {}
        }
    }

    fn on_navigate_back(&mut self, _: &NavigateBack, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_history(true, window, cx);
    }

    fn on_navigate_forward(
        &mut self,
        _: &NavigateForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_history(false, window, cx);
    }

    fn on_switch_session(
        &mut self,
        action: &SwitchSession,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_shortcut(action.0, cx));
    }

    /// Open find-in-page for the persisted conversation on screen.
    fn on_find_transcript(
        &mut self,
        _: &FindTranscript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .terminal_search
            .as_ref()
            .is_some_and(|search| self.terminals.active_id().as_ref() == Some(&search.terminal))
        {
            self.terminal_search
                .as_ref()
                .expect("the terminal search was just checked")
                .query
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
            return;
        }
        if self.terminal_focus.is_focused(window) && self.terminals.active_id().is_some() {
            self.open_terminal_search(window, cx);
            return;
        }
        if self
            .session
            .as_ref()
            .and_then(|row| row.session.as_ref())
            .is_none()
        {
            return;
        }
        if let Some(search) = self.transcript_search.as_ref() {
            search.query.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("transcript.search.placeholder").to_string())
        });
        cx.subscribe(&query, |this, query, event: &InputEvent, cx| match event {
            InputEvent::Change => this.search_transcript(query.read(cx).value().to_string(), cx),
            InputEvent::PressEnter { shift, .. } => this.step_transcript_search(
                if *shift {
                    ginka_ui::search::Direction::Previous
                } else {
                    ginka_ui::search::Direction::Next
                },
                cx,
            ),
            _ => {}
        })
        .detach();
        query.read(cx).focus_handle(cx).focus(window, cx);
        self.transcript_search = Some(TranscriptSearch {
            query,
            typed: String::new(),
            matches: Vec::new(),
            chosen: None,
            loading: false,
            error: None,
        });
        cx.notify();
    }

    /// Ask the daemon for matches, discarding a reply after the query moved on.
    fn search_transcript(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(search) = self.transcript_search.as_mut() else {
            return;
        };
        search.typed = query.clone();
        search.matches.clear();
        search.chosen = None;
        search.error = None;
        if query.trim().is_empty() {
            search.loading = false;
            cx.notify();
            return;
        }
        let Some((session, workspace)) = self.session.as_ref().and_then(|row| {
            row.session
                .as_ref()
                .map(|session| (session.clone(), row.workspace.clone()))
        }) else {
            return;
        };
        search.loading = true;
        cx.notify();
        let link = self.link.clone();
        let expected_query = query.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.search_sessions(workspace, query).await })
                .await;
            this.update(cx, |this, cx| {
                let still_open =
                    this.session.as_ref().and_then(|row| row.session.as_ref()) == Some(&session);
                let Some(search) = this.transcript_search.as_mut() else {
                    return;
                };
                if !still_open || search.typed != expected_query {
                    return;
                }
                search.loading = false;
                match result {
                    Ok(matches) => {
                        search.matches = ginka_ui::search::matches_for_session(matches, &session);
                        search.chosen = (!search.matches.is_empty()).then_some(0);
                        search.error = None;
                    }
                    Err(error) => search.error = Some(error),
                }
                this.scroll_to_search_hit();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Move to another match and keep the selected block in view.
    fn step_transcript_search(
        &mut self,
        direction: ginka_ui::search::Direction,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.transcript_search.as_mut() else {
            return;
        };
        search.chosen = ginka_ui::search::step(search.matches.len(), search.chosen, direction);
        self.scroll_to_search_hit();
        cx.notify();
    }

    /// Scroll the persisted match's folded transcript block into view.
    fn scroll_to_search_hit(&mut self) {
        let Some(seq) = self.transcript_search.as_ref().and_then(|search| {
            search
                .chosen
                .and_then(|index| search.matches.get(index))
                .map(|found| found.seq)
        }) else {
            return;
        };
        if let Some(index) = self.transcript.block_index_for_seq(seq) {
            self.transcript_follows = false;
            self.transcript_scroll.scroll_to_top_of_item(index);
        }
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
        let selected_text = TextSelection::selected_text(window, cx);
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
            selected_text: ginka_ui::transcript::selected_quote(&selected_text).map(str::to_owned),
        });
        cx.notify();
    }

    /// The entries the palette is offering, in the order it offers them.
    fn palette_entries(&self, cx: &App) -> Vec<ginka_ui::palette::Entry> {
        let Some(palette) = self.palette.as_ref() else {
            return Vec::new();
        };
        let rows = self.sidebar.read(cx).rows().to_vec();
        let (can_go_back, can_go_forward) = self.navigation_capabilities(cx);
        let mut entries = ginka_ui::palette::entries(
            &self.layout,
            &rows,
            self.session.as_ref().map(|row| row.indexed),
            self.session
                .as_ref()
                .and_then(|row| row.session.as_ref())
                .is_some(),
            can_go_back,
            can_go_forward,
        );
        let active = self.terminals.active();
        entries.extend(ginka_ui::palette::terminal_entries(
            ginka_ui::palette::TerminalActions {
                tabs: self.terminals.tabs().len(),
                has_output: active.is_some_and(|tab| !tab.screen.text().is_empty()),
                has_selection: self.quoteable_terminal_selection().is_some(),
                split: self.terminals.split_ids().is_some(),
                browsing_history: active.is_some_and(|tab| tab.screen.display_offset() > 0),
                close_armed: active.is_some_and(|tab| self.terminals.close_confirmation(&tab.id)),
            },
        ));
        entries.extend(ginka_ui::palette::transcript_entries(
            self.session.is_some() && palette.selected_text.is_some(),
        ));
        ginka_ui::palette::filter(entries, &palette.typed)
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
                    let selected_text = self
                        .palette
                        .as_ref()
                        .and_then(|palette| palette.selected_text.clone());
                    self.palette = None;
                    self.run_command(command, selected_text.as_deref(), window, cx);
                }
            }
            _ => {}
        }
    }

    /// Do what a palette entry says.
    fn run_command(
        &mut self,
        command: ginka_ui::palette::Command,
        selected_text: Option<&str>,
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
            Command::ToggleTerminalSplit => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.toggle_terminal_split(window, cx);
            }
            Command::FocusOtherTerminalPane => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.focus_other_terminal_pane(window, cx);
            }
            Command::FindTerminal => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.open_terminal_search(window, cx);
            }
            Command::CopyTerminalOutput => self.copy_terminal_output(cx),
            Command::QuoteTerminalSelection => self.quote_terminal_selection(window, cx),
            Command::QuoteTranscriptSelection => {
                if let Some(selection) = selected_text {
                    self.quote_in_composer(selection, window, cx);
                }
            }
            Command::NextTerminal => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.cycle_terminal_tab(false, cx);
                self.terminal_focus.focus(window, cx);
            }
            Command::PreviousTerminal => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.cycle_terminal_tab(true, cx);
                self.terminal_focus.focus(window, cx);
            }
            Command::TerminalToLive => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                self.terminal_to_live(cx);
                self.terminal_focus.focus(window, cx);
            }
            Command::CloseTerminal => {
                if !self.layout.is_open(Panel::TerminalDock) {
                    self.toggle(Panel::TerminalDock, cx);
                }
                if let Some(terminal) = self.terminals.active_id() {
                    self.request_terminal_close(terminal, cx);
                }
            }
            Command::IndexWorkspace => self.index_workspace(window, cx),
            Command::AttachFiles => self.choose_attachments(window, cx),
            Command::FindTranscript => self.on_find_transcript(&FindTranscript, window, cx),
            Command::TogglePromptOutline => {
                self.prompt_outline_open = !self.prompt_outline_open;
            }
            Command::NavigateBack => self.navigate_history(true, window, cx),
            Command::NavigateForward => self.navigate_history(false, window, cx),
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
        // Nothing here borrows `cx`: without `use<>` the built control would,
        // and a header that holds one could not bind the next one.
    ) -> impl IntoElement + use<> {
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

    /// Make a strip the window can be dragged by.
    ///
    /// There is no title bar to do it (`docs/ui.md` §3.1), so each column's
    /// header does. A drag is a press that then moved: acting on the press
    /// alone would carry the window off whenever a control on the strip was
    /// clicked.
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

    /// The window's own controls, across the top of the leading column.
    ///
    /// Room for the traffic lights, then the sidebar toggle and the history
    /// arrows. It sits on the sidebar while there is one and moves onto the
    /// centre column when the sidebar is closed, because the lights do not
    /// move with it.
    fn window_controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (can_go_back, can_go_forward) = self.navigation_capabilities(cx);

        let strip = h_flex()
            .id("window-controls")
            .flex_shrink_0()
            .h(HEADER_HEIGHT)
            .pl(TRAFFIC_LIGHT_INSET)
            .gap_3()
            .items_center()
            .child(self.panel_toggle(
                Panel::Sidebar,
                IconName::PanelLeftClose,
                IconName::PanelLeftOpen,
                cx,
            ))
            .child(
                Button::new("navigation-back")
                    .ghost()
                    .compact()
                    .disabled(!can_go_back)
                    .tooltip(rust_i18n::t!("navigation.back").to_string())
                    .child(Icon::new(IconName::ArrowLeft).size_4())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.navigate_history(true, window, cx)),
                    ),
            )
            .child(
                Button::new("navigation-forward")
                    .ghost()
                    .compact()
                    .disabled(!can_go_forward)
                    .tooltip(rust_i18n::t!("navigation.forward").to_string())
                    .child(Icon::new(IconName::ArrowRight).size_4())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.navigate_history(false, window, cx)),
                    ),
            );

        self.draggable(strip, cx)
    }

    /// The strip across the top of the centre column: what the conversation is,
    /// and the controls for the panels either side of it. `docs/ui.md` §3.1.
    ///
    /// It carries the window controls too when the sidebar is closed, which is
    /// the only arrangement where the centre column is the leading one.
    fn column_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Copied out: the toggles below need `cx` mutably to bind their
        // listeners, and a borrow of the theme held across that is a borrow
        // held across the whole strip.
        let tokens = Tokens::global(cx).clone();
        let muted = tokens.colors().text_muted;
        let secondary = tokens.colors().text_secondary;
        let primary = tokens.colors().text_primary;
        let leading = !self.layout.is_open(Panel::Sidebar);
        let session = self.session.clone();
        // The conversation names itself; without one the project it would run
        // in does. With neither, nothing: an empty window is not an error, and
        // a placeholder title would be the only thing claiming otherwise.
        let title: Option<SharedString> = session
            .as_ref()
            .map(|session| session.title.clone())
            .or_else(|| self.project_label());
        let origin: Option<SharedString> = session.as_ref().map(|session| session.origin.clone());
        let glyph = session.as_ref().map(|session| session.agent.glyph());
        // Only when the centre column is the leading one, which is the one
        // arrangement where the traffic lights sit over it.
        let sidebar_toggle = leading.then(|| {
            self.panel_toggle(
                Panel::Sidebar,
                IconName::PanelLeftClose,
                IconName::PanelLeftOpen,
                cx,
            )
        });
        let new_chat = div()
            .id("header-new-chat")
            .p_1()
            .rounded(px(tokens.radius.row))
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().bg_raised))
            .child(Icon::new(IconName::Plus).size_4().text_color(secondary))
            .on_click(cx.listener(|this, _, window, cx| {
                let project = this.target_project.clone();
                this.start_new_chat(project, window, cx);
            }));
        let dock_toggle = self.panel_toggle(
            Panel::TerminalDock,
            IconName::PanelBottom,
            IconName::PanelBottomOpen,
            cx,
        );
        let right_toggle = self.panel_toggle(
            Panel::RightPanel,
            IconName::PanelRightClose,
            IconName::PanelRightOpen,
            cx,
        );

        let strip =
            h_flex()
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
                        .children(glyph.map(|glyph| glyph.size_4().text_color(secondary)))
                        .children(title.map(|title| {
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(primary)
                                .truncate()
                                .child(title)
                        }))
                        .children(origin.map(|origin| {
                            div().text_xs().text_color(muted).truncate().child(origin)
                        })),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(new_chat)
                        .child(dock_toggle)
                        .child(right_toggle),
                );

        self.draggable(strip, cx)
    }

    /// The conversation, folded from the daemon's event stream — or the home
    /// screen, when there is not one yet.
    ///
    /// An empty column is the ordinary way this window opens, so it is a front
    /// door rather than an apology. Once a prompt is away the scrolling column
    /// takes over even before the first word arrives, because that is where
    /// the activity line lives and "working" is what the reader needs to see.
    fn transcript(&self, selected_text: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        if self.transcript.is_empty() && !self.is_working() {
            return self.home(cx);
        }
        let hit = self.transcript_search.as_ref().and_then(|search| {
            search
                .chosen
                .and_then(|chosen| search.matches.get(chosen))
                .and_then(|found| self.transcript.block_index_for_seq(found.seq))
        });
        let scroller = v_flex()
            .id("transcript-scroll")
            .flex_1()
            .px_8()
            .py_6()
            .overflow_y_scroll()
            .track_scroll(&self.transcript_scroll)
            // The gesture, not the resulting offset: an answer that grows
            // moves the foot away from the reader too.
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.transcript_scrolled()))
            .children({
                let last = self.transcript.blocks().len().saturating_sub(1);
                self.transcript
                    .blocks()
                    .iter()
                    .enumerate()
                    .map(|(index, block)| {
                        let block = match block {
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
                        };
                        // Each block is a direct child of the scroller so its
                        // persisted sequence can be brought into view by ⌘F.
                        div()
                            .w_full()
                            .max_w(px(TRANSCRIPT_MEASURE))
                            .mx_auto()
                            .mb_5()
                            .when(hit == Some(index), |this| {
                                this.rounded(px(Tokens::global(cx).radius.card))
                                    .bg(Tokens::global(cx).colors().row_active())
                            })
                            .child(block)
                    })
                    .collect::<Vec<_>>()
            })
            .child(
                div()
                    .w_full()
                    .max_w(px(TRANSCRIPT_MEASURE))
                    .mx_auto()
                    .children(self.activity_line(cx)),
            );
        v_flex()
            .id("transcript")
            .relative()
            .flex_1()
            .min_h_0()
            .children(self.transcript_outline(cx))
            .children(self.transcript_search_bar(cx))
            .child(scroller)
            .children(self.transcript_selection_action(selected_text, cx))
            .into_any_element()
    }

    /// A selection-scoped quote action that does not move the transcript.
    ///
    /// A mouse press clears the toolkit's window selection before its click is
    /// delivered, so the action retains the render-time text and handles mouse
    /// activation on press. Keyboard and touch activation keep the ordinary
    /// button click path.
    fn transcript_selection_action(
        &self,
        selected_text: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let selected_text = selected_text?;
        let mouse_text = selected_text.clone();
        let click_text = selected_text;
        let label = rust_i18n::t!("transcript.quote_selection").to_string();

        Some(
            div()
                .absolute()
                .top_2()
                .right_3()
                .child(
                    Button::new("quote-transcript-selection")
                        .compact()
                        .tooltip(label.clone())
                        .accessibility_label(label.clone())
                        .label(label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.quote_in_composer(&mouse_text, window, cx)
                            }),
                        )
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if !matches!(event, ClickEvent::Mouse(_)) {
                                this.quote_in_composer(&click_text, window, cx);
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    /// A bounded list of the conversation's top-level prompts.
    ///
    /// The folded transcript supplies drawable block indexes, so choosing an
    /// item uses the same scroll path as persisted search without asking the
    /// daemon to rediscover text already on screen.
    fn transcript_outline(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        const LABEL_CHARS: usize = 72;

        let prompts = self
            .transcript
            .prompt_outline()
            .enumerate()
            .map(|(number, (block, prompt))| {
                (
                    number + 1,
                    block,
                    ginka_ui::transcript::prompt_outline_label(prompt, LABEL_CHARS),
                )
            })
            .collect::<Vec<_>>();
        if prompts.is_empty() {
            return None;
        }

        let tokens = Tokens::global(cx).clone();
        let count = prompts.len();
        let open = self.prompt_outline_open;
        let rows = open.then(|| {
            v_flex()
                .w_full()
                .max_h(px(184.))
                .px_3()
                .pb_2()
                .gap_0p5()
                .overflow_y_scrollbar()
                .children(prompts.into_iter().map(|(number, block, label)| {
                    Button::new(SharedString::from(format!("prompt-outline-{block}")))
                        .ghost()
                        .w_full()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(24.))
                                        .text_xs()
                                        .text_color(tokens.colors().text_muted)
                                        .child(number.to_string()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .truncate()
                                        .text_sm()
                                        .text_color(tokens.colors().text_secondary)
                                        .child(label),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.transcript_follows = false;
                            this.transcript_scroll.scroll_to_top_of_item(block);
                            cx.notify();
                        }))
                }))
        });

        Some(
            v_flex()
                .w_full()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .bg(tokens.colors().bg_surface)
                .child(
                    h_flex().w_full().h(px(34.)).px_3().justify_end().child(
                        Button::new("toggle-prompt-outline")
                            .ghost()
                            .compact()
                            .tooltip(rust_i18n::t!("transcript.outline.tooltip").to_string())
                            .child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        Icon::new(if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .size_3(),
                                    )
                                    .child(
                                        rust_i18n::t!("transcript.outline.label", count = count)
                                            .to_string(),
                                    ),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prompt_outline_open = !this.prompt_outline_open;
                                cx.notify();
                            })),
                    ),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// Find-in-page controls above the conversation.
    fn transcript_search_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let search = self.transcript_search.as_ref()?;
        let tokens = Tokens::global(cx).clone();
        let count = match (&search.error, search.loading, search.chosen) {
            (Some(_), _, _) => rust_i18n::t!("transcript.search.failed").to_string(),
            (_, true, _) => rust_i18n::t!("transcript.search.searching").to_string(),
            (_, false, Some(chosen)) => format!("{} / {}", chosen + 1, search.matches.len()),
            _ => format!("0 / {}", search.matches.len()),
        };
        let excerpt = search
            .chosen
            .and_then(|chosen| search.matches.get(chosen))
            .map(|found| found.excerpt.clone())
            .or_else(|| search.error.clone());
        Some(
            v_flex()
                .w_full()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .bg(tokens.colors().bg_surface)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.transcript_search = None;
                        cx.notify();
                    }
                }))
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .items_center()
                        .child(div().flex_1().child(Input::new(&search.query)))
                        .child(
                            div()
                                .min_w(px(58.))
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .child(count),
                        )
                        .child(
                            Button::new("previous-transcript-match")
                                .ghost()
                                .disabled(search.matches.is_empty())
                                .tooltip(rust_i18n::t!("transcript.search.previous").to_string())
                                .child(Icon::new(IconName::ArrowUp).size_3())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step_transcript_search(
                                        ginka_ui::search::Direction::Previous,
                                        cx,
                                    )
                                })),
                        )
                        .child(
                            Button::new("next-transcript-match")
                                .ghost()
                                .disabled(search.matches.is_empty())
                                .tooltip(rust_i18n::t!("transcript.search.next").to_string())
                                .child(Icon::new(IconName::ArrowDown).size_3())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step_transcript_search(
                                        ginka_ui::search::Direction::Next,
                                        cx,
                                    )
                                })),
                        )
                        .child(
                            Button::new("close-transcript-search")
                                .ghost()
                                .tooltip(rust_i18n::t!("transcript.search.close").to_string())
                                .child(Icon::new(IconName::Close).size_3())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.transcript_search = None;
                                    cx.notify();
                                })),
                        ),
                )
                .children(excerpt.map(|excerpt| {
                    div()
                        .w_full()
                        .truncate()
                        .text_xs()
                        .text_color(if search.error.is_some() {
                            tokens.colors().status_error
                        } else {
                            tokens.colors().text_secondary
                        })
                        .child(excerpt)
                }))
                .into_any_element(),
        )
    }

    /// The home screen: what the centre column asks before there is a
    /// conversation in it. `docs/ui.md` §3.3.
    ///
    /// One question, naming the project the answer would run in, over four
    /// starters. Choosing one fills the composer rather than sending it: the
    /// starter is the first half of a sentence the reader finishes.
    fn home(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let project = self.project_label();
        let starters = home::starters();

        v_flex()
            .id("home")
            .flex_1()
            .px_8()
            .py_6()
            .gap(px(28.))
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_size(px(30.))
                    .line_height(px(40.))
                    .text_color(tokens.colors().text_primary)
                    .child(home::greeting(project.as_deref())),
            )
            .child(
                h_flex()
                    .w_full()
                    .max_w(px(TRANSCRIPT_MEASURE))
                    // The composer's own padding, so the cards line up with
                    // the box the reader types into rather than sitting a
                    // hair wider than it.
                    .px_4()
                    .gap_3()
                    .items_stretch()
                    .children(starters.into_iter().map(|starter| {
                        let prompt = starter.prompt.clone();
                        v_flex()
                            .id(SharedString::from(format!("starter:{}", starter.id)))
                            .flex_1()
                            // Without this the four labels' min-content widths
                            // add up to more than the measure and push the row
                            // wider than the composer under it. They wrap
                            // instead.
                            .min_w(px(0.))
                            .h(px(104.))
                            .p_3()
                            .gap_2()
                            .rounded(px(tokens.radius.card))
                            .bg(tokens.colors().bg_surface)
                            .border_1()
                            .border_color(tokens.colors().border_subtle)
                            .cursor_pointer()
                            .hover(|this| this.border_color(tokens.colors().border_strong))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.use_starter(prompt.clone(), window, cx)
                            }))
                            .child(
                                Icon::empty()
                                    .path(starter.icon)
                                    .size_4()
                                    .text_color(tokens.colors().accent),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .text_color(tokens.colors().text_secondary)
                                    .child(starter.label),
                            )
                    })),
            )
            .into_any_element()
    }

    /// Put a starter in the composer, and leave the cursor in it.
    fn use_starter(&mut self, prompt: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |state, cx| state.set_value(prompt, window, cx));
        self.composer.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// A question the agent is waiting on, with its answers as buttons.
    ///
    /// The options are the agent's own words, so they are sent back verbatim:
    /// a card that paraphrased what the reader chose would be answering a
    /// different question. Once answered it is history, and history is read
    /// rather than clicked.
    fn asked(
        &self,
        index: usize,
        id: &str,
        question: &str,
        options: &[String],
        answered: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(px(tokens.radius.card))
            .bg(tokens.colors().bg_surface)
            .border_1()
            .border_color(if answered {
                tokens.colors().border_subtle
            } else {
                tokens.colors().status_attention.opacity(0.55)
            })
            .child(
                div()
                    .text_size(px(15.))
                    .text_color(tokens.colors().text_primary)
                    .child(question.to_string()),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(options.iter().enumerate().map(|(at, option)| {
                        let answer = option.clone();
                        let asked = id.to_string();
                        div()
                            .id(SharedString::from(format!("ask-{index}-{at}")))
                            .px_2p5()
                            .py_1()
                            .rounded(px(tokens.radius.row))
                            .text_sm()
                            .when(answered, |this| {
                                this.text_color(tokens.colors().text_muted)
                                    .border_1()
                                    .border_color(tokens.colors().border_subtle)
                            })
                            .when(!answered, |this| {
                                this.bg(tokens.colors().row_active())
                                    .text_color(tokens.colors().text_primary)
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().accent.opacity(0.35)))
                            })
                            .when(!answered, |this| {
                                this.on_click(cx.listener(move |this, _, _, cx| {
                                    this.respond(asked.clone(), answer.clone(), cx)
                                }))
                            })
                            .child(option.clone())
                    })),
            )
            .into_any_element()
    }

    /// A plan the agent wants approved, with the two answers it can take.
    fn proposed(
        &self,
        index: usize,
        id: &str,
        plan: &str,
        answered: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let approve = id.to_string();
        let reject = id.to_string();
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(px(tokens.radius.card))
            .bg(tokens.colors().bg_surface)
            .border_1()
            .border_color(if answered {
                tokens.colors().border_subtle
            } else {
                tokens.colors().accent.opacity(0.55)
            })
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(25.))
                    .text_color(tokens.colors().text_primary)
                    .child(plan.to_string()),
            )
            .children((!answered).then(|| {
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("plan-yes-{index}")))
                            .px_2p5()
                            .py_1()
                            .rounded(px(tokens.radius.row))
                            .bg(tokens.colors().accent.opacity(0.9))
                            .text_sm()
                            .text_color(tokens.colors().bg_window)
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().accent))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                // English, because this one is read by the
                                // agent rather than by the user: the label
                                // beside it is what the user reads.
                                this.respond(
                                    approve.clone(),
                                    "Approved. Go ahead with this plan.".into(),
                                    cx,
                                )
                            }))
                            .child(rust_i18n::t!("transcript.plan.approve").to_string()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("plan-no-{index}")))
                            .px_2p5()
                            .py_1()
                            .rounded(px(tokens.radius.row))
                            .text_sm()
                            .text_color(tokens.colors().text_muted)
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.respond(
                                    reject.clone(),
                                    "Not approved. Stop and wait for further instructions.".into(),
                                    cx,
                                )
                            }))
                            .child(rust_i18n::t!("transcript.plan.reject").to_string()),
                    )
            }))
            .into_any_element()
    }

    /// Answer whatever the agent is waiting on.
    fn respond(&mut self, request_id: String, response: String, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            return;
        };
        let link = self.link.clone();
        let answered_request = request_id.clone();
        cx.spawn(async move |this, cx| {
            let answered = cx
                .background_spawn(
                    async move { link.respond(&session, &request_id, &response).await },
                )
                .await;
            if answered.is_ok() {
                this.update(cx, |this, cx| {
                    this.transcript.answer(&answered_request);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Deliver a free-text interaction answer from the composer.
    ///
    /// The text and its draft stay put if the daemon rejects a stale card or
    /// the transport disappears. Only an acknowledged delivery clears them.
    fn respond_from_composer(
        &mut self,
        session: SessionId,
        request_id: String,
        response: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = self.link.clone();
        let workspace = self.session.as_ref().map(|row| row.workspace.clone());
        self.submitted = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let answered = cx
                .background_spawn(
                    async move { link.respond(&session, &request_id, &response).await },
                )
                .await;
            let clear_draft = answered.is_ok();
            this.update_in(cx, |this, window, cx| {
                this.submitted = false;
                if clear_draft {
                    this.composer
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                cx.notify();
            })
            .ok();
            if clear_draft && let Some(workspace) = workspace {
                let link = this.update(cx, |this, _| this.link.clone()).ok();
                if let Some(link) = link {
                    cx.background_spawn(
                        async move { link.save_draft(&workspace, String::new()).await },
                    )
                    .await;
                }
            }
        })
        .detach();
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
            TranscriptBlock::User { text } => {
                v_flex()
                    .w_full()
                    .items_end()
                    .gap_1()
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
                    .child(self.message_actions(index, "user", text, cx))
                    .into_any_element()
            }
            TranscriptBlock::Assistant { text } => {
                // Agents answer in markdown — headings, lists, fenced code —
                // and reading it raw is reading the punctuation instead of the
                // answer. Only the finished blocks are formatted: markdown of
                // half a document re-flows the text under the reader on every
                // frame, so the line still being written is drawn as the plain
                // text it is until its block is done.
                let (formatted, writing) = ginka_ui::transcript::settled(text);
                let linked = Arc::new(
                    self.session
                        .as_ref()
                        .map(|row| ginka_ui::transcript::link_file_locations(formatted, &row.path))
                        .unwrap_or_else(|| ginka_ui::transcript::LinkedMarkdown {
                            markdown: formatted.to_string(),
                            targets: Vec::new(),
                        }),
                );
                let linked_click = linked.clone();
                let shell = cx.entity().downgrade();
                v_flex()
                    .w_full()
                    .gap_1()
                    .text_size(px(15.))
                    .line_height(px(25.))
                    .text_color(tokens.colors().text_primary)
                    .children((!formatted.is_empty()).then(|| {
                        TextView::markdown(("assistant", index), linked.markdown.clone())
                            .selectable(true)
                            .on_link_click(move |href, event, window, cx| {
                                if event.is_right_click() {
                                    return;
                                }
                                let Some(target) = linked_click.target(href).cloned() else {
                                    cx.open_url(href);
                                    return;
                                };
                                let path = target.path.clone();
                                let _ = shell.update(cx, |this, cx| {
                                    if let Some(range) = target.selection_range()
                                        && let Some(workspace) =
                                            this.session.as_ref().map(|row| row.workspace.clone())
                                    {
                                        this.open_definition(
                                            &workspace,
                                            ginka_ui::editor::DefinitionTarget {
                                                path: path.clone(),
                                                range,
                                            },
                                            window,
                                            cx,
                                        );
                                    } else {
                                        this.open_file(path.clone(), false, window, cx);
                                    }
                                });
                            })
                    }))
                    .children((!writing.is_empty()).then(|| div().child(writing.to_string())))
                    .child(self.message_actions(index, "assistant", text, cx))
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
            TranscriptBlock::Tasks { items } => self.task_card(items, cx),
            TranscriptBlock::Subagent {
                title,
                steps,
                summary,
                is_error,
                ..
            } => self.subagent_card(title, steps, summary.as_deref(), *is_error, cx),
            TranscriptBlock::Question {
                id,
                question,
                options,
                answered,
            } => self.asked(index, id, question, options, *answered, cx),
            TranscriptBlock::Plan { id, plan, answered } => {
                self.proposed(index, id, plan, *answered, cx)
            }
            // A turn boundary is where a checkpoint was taken, which is what
            // makes it worth drawing — and what makes it the way back.
            TranscriptBlock::TurnEnd {
                turn,
                seq,
                provider,
                model,
                reasoning_effort,
                service_tier,
            } => {
                let provenance = ginka_ui::transcript::turn_provenance(
                    provider.as_deref(),
                    model.as_deref(),
                    reasoning_effort.as_deref(),
                    service_tier.as_deref(),
                );
                self.turn_rule(*turn, *seq, provenance, cx)
            }
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

    /// Copy the conversation through `after` onto another agent and show it.
    fn fork_from(&mut self, after: u64, agent: String, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().and_then(|row| row.session.clone()) else {
            return;
        };
        let Some(menu) = self.forking.as_mut() else {
            return;
        };
        if menu.busy {
            return;
        }
        menu.busy = true;
        menu.error = None;
        cx.notify();

        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let requesting = link.clone();
            let result = cx
                .background_spawn(
                    async move { requesting.fork_session(&session, after, agent).await },
                )
                .await;
            match result {
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if let Some(menu) = this.forking.as_mut() {
                            menu.busy = false;
                            menu.error = Some(
                                rust_i18n::t!("transcript.fork.failed", error = error).to_string(),
                            );
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Ok(fork) => {
                    this.update(cx, |this, cx| {
                        this.transcript = Transcript::new();
                        this.transcript_of = None;
                        this.transcript_search = None;
                        this.session_state = Some(fork.state);
                        this.checkpoints.clear();
                        this.rewinding = None;
                        this.forking = None;
                        this.chosen_agent = None;
                        this.chosen_model = None;
                        this.chosen_reasoning_effort = None;
                        this.chosen_service_tier = None;
                        this.chosen_account = None;
                        cx.notify();
                    })
                    .ok();
                    if let Ok(Some(showing)) = pull_rows(&this, &link, cx).await
                        && showing == fork.id
                    {
                        let _ = pull_transcript(&this, &link, showing, cx).await;
                    }
                }
            }
        })
        .detach();
    }

    /// The rule between turns, and the ways to rewind or continue elsewhere.
    ///
    /// A transcript position maps both to a checkpointed working tree and to
    /// the inclusive event range a fork copies. Rewind is confirmed because it
    /// changes files; a fork is additive and can run immediately.
    fn turn_rule(
        &self,
        turn: u32,
        seq: u64,
        provenance: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let checkpoint = self
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.turn == turn)
            .map(|checkpoint| checkpoint.id.clone());
        let asking = self.rewinding == Some(turn);
        let current_agent = self
            .session
            .as_ref()
            .map(|row| row.agent.driver_id())
            .unwrap_or_default();
        let targets = ginka_ui::handoff::fork_targets(&self.agents, current_agent);
        let has_targets = !targets.is_empty();
        let (fork_open, fork_busy, fork_error) = self
            .forking
            .as_ref()
            .filter(|menu| menu.turn == turn && menu.seq == seq)
            .map(|menu| (true, menu.busy, menu.error.clone()))
            .unwrap_or((false, false, None));
        let target_buttons = targets.into_iter().map(|agent| {
            let id = agent.id.clone();
            Button::new(SharedString::from(format!("fork-{turn}-{id}")))
                .disabled(fork_busy)
                .px(px(7.))
                .py(px(2.))
                .rounded(px(tokens.radius.row))
                .text_xs()
                .text_color(tokens.colors().text_primary)
                .when(!fork_busy, |this| {
                    this.cursor_pointer()
                        .hover(|this| this.bg(tokens.colors().row_hover()))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.fork_from(seq, id.clone(), cx)),
                        )
                })
                .child(agent.display_name.clone())
                .into_any_element()
        });
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
            .children(provenance.map(|provenance| {
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted.opacity(0.7))
                    .child(provenance)
            }))
            .children(has_targets.then(|| {
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new(SharedString::from(format!("fork-menu-{turn}")))
                            .disabled(fork_busy)
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(tokens.radius.row))
                            .text_xs()
                            .text_color(if fork_open {
                                tokens.colors().text_primary
                            } else {
                                tokens.colors().text_muted.opacity(0.7)
                            })
                            .when(!fork_busy, |this| {
                                this.cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .tooltip(rust_i18n::t!("transcript.fork.hint").to_string())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.rewinding = None;
                                        this.forking =
                                            if this.forking.as_ref().is_some_and(|menu| {
                                                menu.turn == turn && menu.seq == seq
                                            }) {
                                                None
                                            } else {
                                                Some(ForkMenu {
                                                    turn,
                                                    seq,
                                                    busy: false,
                                                    error: None,
                                                })
                                            };
                                        cx.notify();
                                    }))
                            })
                            .child(if fork_busy {
                                rust_i18n::t!("transcript.fork.working").to_string()
                            } else {
                                rust_i18n::t!("transcript.fork.label").to_string()
                            }),
                    )
                    .children(fork_open.then(|| h_flex().gap_1().children(target_buttons)))
                    .children(fork_error.map(|error| {
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(error)
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

    /// The latest provider-neutral task snapshot for the current turn.
    fn task_card(&self, items: &[ginka_protocol::TaskItem], cx: &App) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let (completed, total) = ginka_ui::transcript::task_progress(items);

        v_flex()
            .w_full()
            .rounded(px(tokens.radius.row))
            .border_1()
            .border_color(tokens.colors().border_subtle)
            .bg(tokens.colors().bg_surface)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_medium()
                            .text_color(tokens.colors().text_secondary)
                            .child(rust_i18n::t!("transcript.tasks").to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(format!("{completed}/{total}")),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(tokens.colors().border_subtle)
                    .children(items.iter().map(|item| {
                        let (mark, colour) = match item.status {
                            TaskStatus::Pending => ("○", tokens.colors().text_muted),
                            TaskStatus::InProgress => ("→", tokens.colors().status_working),
                            TaskStatus::Completed => ("✓", tokens.colors().status_done),
                            TaskStatus::Cancelled => ("×", tokens.colors().text_muted.opacity(0.7)),
                        };
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .text_color(colour)
                            .child(div().min_w(px(14.)).child(mark))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(if item.status == TaskStatus::Cancelled {
                                        tokens.colors().text_muted
                                    } else {
                                        tokens.colors().text_primary
                                    })
                                    .child(item.label.clone()),
                            )
                    })),
            )
            .into_any_element()
    }

    /// One delegated run, with its newest work kept under the row that spawned it.
    fn subagent_card(
        &self,
        title: &str,
        steps: &[ginka_protocol::SubagentStep],
        summary: Option<&str>,
        is_error: bool,
        cx: &App,
    ) -> AnyElement {
        const VISIBLE_STEPS: usize = 12;

        let tokens = Tokens::global(cx).clone();
        let hidden = steps.len().saturating_sub(VISIBLE_STEPS);
        let shown = steps.iter().skip(hidden);
        let state = if summary.is_none() {
            rust_i18n::t!("transcript.subagent.running").to_string()
        } else if is_error {
            rust_i18n::t!("transcript.subagent.failed").to_string()
        } else {
            rust_i18n::t!("transcript.subagent.completed").to_string()
        };

        v_flex()
            .w_full()
            .rounded(px(tokens.radius.row))
            .border_1()
            .border_color(if is_error {
                tokens.colors().status_error.opacity(0.55)
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
                            .font_medium()
                            .text_color(tokens.colors().text_secondary)
                            .child(rust_i18n::t!("transcript.subagent").to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if is_error {
                                tokens.colors().status_error
                            } else {
                                tokens.colors().text_muted
                            })
                            .child(state),
                    ),
            )
            .when(!steps.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_1p5()
                        .border_t_1()
                        .border_color(tokens.colors().border_subtle)
                        .children((hidden > 0).then(|| {
                            div()
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .child(
                                    rust_i18n::t!("transcript.subagent.earlier", count = hidden)
                                        .to_string(),
                                )
                        }))
                        .children(shown.map(|step| {
                            let (mark, color) = match step.status {
                                Some(SubagentStepStatus::Running) => {
                                    ("·", tokens.colors().status_working)
                                }
                                Some(SubagentStepStatus::Completed) => {
                                    ("✓", tokens.colors().text_muted)
                                }
                                Some(SubagentStepStatus::Failed) => {
                                    ("×", tokens.colors().status_error)
                                }
                                None => ("›", tokens.colors().text_muted),
                            };
                            h_flex()
                                .w_full()
                                .gap_2()
                                .items_start()
                                .child(div().w_3().text_xs().text_color(color).child(mark))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .line_height(px(18.))
                                        .text_color(tokens.colors().text_secondary)
                                        .child(step.text.clone()),
                                )
                        })),
                )
            })
            .children(
                summary
                    .filter(|summary| !summary.is_empty())
                    .map(|summary| {
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .border_t_1()
                            .border_color(tokens.colors().border_subtle)
                            .text_xs()
                            .line_height(px(18.))
                            .text_color(if is_error {
                                tokens.colors().status_error
                            } else {
                                tokens.colors().text_secondary
                            })
                            .child(summary.to_string())
                    }),
            )
            .into_any_element()
    }

    /// The editable FIFO shown above the composer while a turn is active.
    fn queue_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.queued_messages.is_empty() && self.queue_error.is_none() {
            return None;
        }
        let tokens = Tokens::global(cx).clone();
        let count = self.queued_messages.len();
        let rows = self
            .queued_messages
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, message)| {
                let send_id = message.id;
                let edit_id = message.id;
                let edit_text = message.text.clone();
                let remove_id = message.id;
                let earlier_id = message.id;
                let later_id = message.id;
                h_flex()
                    .w_full()
                    .min_h(px(42.))
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(18.))
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(tokens.colors().text_primary)
                            .child(message.text),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-now-{send_id}")))
                            .ghost()
                            .compact()
                            .disabled(!self.queue_can_send_now)
                            .label(rust_i18n::t!("composer.queue.send_now").to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_queued_message_now(send_id, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-earlier-{earlier_id}")))
                            .ghost()
                            .compact()
                            .disabled(index == 0)
                            .tooltip(rust_i18n::t!("composer.queue.earlier").to_string())
                            .child(Icon::new(IconName::ArrowUp).size_3())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_queued_message(
                                    earlier_id,
                                    index.saturating_sub(1) as u32,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-later-{later_id}")))
                            .ghost()
                            .compact()
                            .disabled(index + 1 >= count)
                            .tooltip(rust_i18n::t!("composer.queue.later").to_string())
                            .child(Icon::new(IconName::ArrowDown).size_3())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_queued_message(later_id, (index + 1) as u32, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-edit-{edit_id}")))
                            .ghost()
                            .compact()
                            .label(rust_i18n::t!("composer.queue.edit").to_string())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_queued_message(edit_id, edit_text.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-remove-{remove_id}")))
                            .ghost()
                            .compact()
                            .label(rust_i18n::t!("composer.queue.remove").to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_queued_message(remove_id, cx)
                            })),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            v_flex()
                .w_full()
                .px_3()
                .py_2()
                .gap_1()
                .rounded(px(tokens.radius.card))
                .bg(tokens.colors().bg_surface)
                .border_1()
                .border_color(tokens.colors().border_subtle)
                .child(
                    h_flex().w_full().items_center().child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(tokens.colors().text_secondary)
                            .child(
                                rust_i18n::t!("composer.queue.title", count = count).to_string(),
                            ),
                    ),
                )
                .children(rows)
                .children(self.queue_error.clone().map(|error| {
                    div()
                        .text_xs()
                        .text_color(tokens.colors().status_error)
                        .child(error)
                }))
                .into_any_element(),
        )
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
        let awaiting_input = self.session_state == Some(SessionState::AwaitingInput);
        let accepts_attachments = self.editing_queued_message.is_none()
            && ginka_ui::composer::can_accept_attachments(self.attachment_busy, awaiting_input);
        let primary_action = ginka_ui::composer::primary_action(
            working,
            self.composer.read(cx).value().as_ref(),
            !awaiting_input && !self.attachments.is_empty(),
        );
        let picker = self.picker_panel(cx);
        let model_chip = self.model_chip_button(cx);
        let effort_chip = self.reasoning_effort_chip_button(cx);
        let tier_chip = self.service_tier_chip_button(cx);
        let access_chip = self.access_chip_button(cx);
        let agent_chip = self.agent_chip_button(cx);
        let account_chip = self.account_chip_button(cx);
        let usage_chip = self.usage_chip_button(cx);
        let compact_chip = self.compact_context_button(cx);
        let new_session = self.new_session_button(cx);
        let queue_panel = self.queue_panel(cx);
        let queue_editing = self.editing_queued_message.map(|_| {
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(tokens.colors().text_secondary)
                        .child(rust_i18n::t!("composer.queue.editing").to_string()),
                )
                .child(
                    Button::new("cancel-queue-edit")
                        .ghost()
                        .compact()
                        .label(rust_i18n::t!("composer.queue.cancel").to_string())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_queued_edit(window, cx)),
                        ),
                )
        });
        let attachment_chips = self
            .attachments
            .iter()
            .map(|attachment| {
                let open_reference = attachment.attachment.reference.clone();
                let remove_reference = attachment.attachment.reference.clone();
                let name = attachment.attachment.name.clone();
                let content = h_flex()
                    .gap_1()
                    .items_center()
                    .children(attachment.preview_url.clone().map(|preview_url| {
                        div()
                            .size(px(36.))
                            .rounded(px(tokens.radius.row))
                            .overflow_hidden()
                            .child(
                                img(SharedString::from(preview_url))
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            )
                    }))
                    .child(div().max_w(px(180.)).truncate().text_xs().child(name));
                let content = if attachment.preview_url.is_some() {
                    Button::new(SharedString::from(format!(
                        "annotate-attachment-{open_reference}"
                    )))
                    .ghost()
                    .compact()
                    .tooltip(rust_i18n::t!("composer.markup.open").to_string())
                    .child(content)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_image_markup(&open_reference, window, cx)
                    }))
                    .into_any_element()
                } else {
                    content.into_any_element()
                };
                h_flex()
                    .items_center()
                    .rounded(px(tokens.radius.row))
                    .bg(tokens.colors().row_hover())
                    .child(content)
                    .child(
                        Button::new(SharedString::from(format!(
                            "remove-attachment-{remove_reference}"
                        )))
                        .ghost()
                        .compact()
                        .icon(IconName::Close)
                        .tooltip(rust_i18n::t!("composer.attachment.remove").to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_attachment(&remove_reference, cx)
                        })),
                    )
            })
            .collect::<Vec<_>>();
        let attachment_error = self.attachment_error.clone();

        v_flex()
            .w_full()
            .max_w(px(TRANSCRIPT_MEASURE))
            .mx_auto()
            .px_4()
            .pb_3()
            .gap_2()
            .children(picker)
            .children(queue_panel)
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
                    .when(accepts_attachments, |this| {
                        this.drag_over::<ExternalPaths>(|this, _, _, cx| {
                            this.border_color(Tokens::global(cx).colors().accent)
                                .bg(Tokens::global(cx).colors().row_hover())
                        })
                        .on_drop(cx.listener(
                            |this, paths: &ExternalPaths, _, cx| {
                                this.attach_paths(paths.paths().to_vec(), cx);
                            },
                        ))
                    })
                    .on_action(cx.listener(Self::paste_attachments))
                    .children((!attachment_chips.is_empty()).then(|| {
                        h_flex()
                            .w_full()
                            .gap_1()
                            .flex_wrap()
                            .children(attachment_chips)
                    }))
                    .children(attachment_error.map(|error| {
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(
                                rust_i18n::t!("composer.attachment.failed", error = error)
                                    .to_string(),
                            )
                    }))
                    .children(queue_editing)
                    .child(Textarea::new(&self.composer))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("attach-files")
                                    .ghost()
                                    .compact()
                                    .disabled(self.attachment_busy || awaiting_input)
                                    .tooltip(if self.attachment_busy {
                                        rust_i18n::t!("composer.attachment.uploading").to_string()
                                    } else {
                                        rust_i18n::t!("composer.attachment.add").to_string()
                                    })
                                    .child(
                                        Icon::empty()
                                            .path(ginka_ui::assets::icon::PAPERCLIP)
                                            .size_4()
                                            .text_color(tokens.colors().text_muted),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.choose_attachments(window, cx)
                                    })),
                            )
                            .child(div().flex_1())
                            .children(model_chip)
                            .children(effort_chip)
                            .children(tier_chip)
                            .children(access_chip)
                            .child(agent_chip)
                            .children(account_chip)
                            .children(usage_chip)
                            .children(compact_chip)
                            .child(
                                if primary_action == ginka_ui::composer::PrimaryAction::Stop {
                                    // With no follow-up waiting, stopping is the
                                    // one useful action on a running turn. As
                                    // soon as there is a draft this place turns
                                    // back into Send for steer-or-queue.
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
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit(window, cx)
                                        }))
                                        .child(
                                            Icon::new(IconName::ArrowUp)
                                                .size_4()
                                                .text_color(tokens.colors().bg_window),
                                        )
                                        .into_any_element()
                                },
                            ),
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
        let picker = self.picker?;
        if picker == Picker::Model {
            return self.model_picker_panel(cx);
        }
        if picker == Picker::Branch {
            return self.branch_picker_panel(cx);
        }
        let tokens = Tokens::global(cx);

        let rows: Vec<AnyElement> = match picker {
            // Every project, then the two things that are not one: registering
            // another, and deciding to work without any. Both belong here
            // rather than only in the sidebar — this is where the reader is
            // when they notice the chat is aimed at the wrong place.
            Picker::Project => {
                let mut rows: Vec<AnyElement> = self
                    .projects
                    .iter()
                    .map(|project| {
                        let name = project.name.clone();
                        let chosen = self.target_project.as_ref() == Some(&project.name);
                        self.picker_row(
                            SharedString::from(format!("project-option:{}", project.name.0)),
                            project.label.to_string(),
                            Some(project.path.to_string()),
                            chosen,
                            cx.listener(move |this, _, window, cx| {
                                this.choose_project(Some(name.clone()), window, cx)
                            }),
                            cx,
                        )
                    })
                    .collect();
                if self.local_paths {
                    rows.push(self.picker_row(
                        SharedString::from("project-option:new"),
                        rust_i18n::t!("composer.project.new").to_string(),
                        None,
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.picker = None;
                            this.open_add_project(window, cx);
                        }),
                        cx,
                    ));
                }
                rows.push(self.picker_row(
                    SharedString::from("project-option:none"),
                    rust_i18n::t!("composer.project.none").to_string(),
                    Some(rust_i18n::t!("composer.project.none.note").to_string()),
                    self.target_project.is_none(),
                    cx.listener(|this, _, window, cx| this.choose_project(None, window, cx)),
                    cx,
                ));
                rows
            }
            Picker::Agent => self.agent_rows(cx),
            Picker::Branch => unreachable!("the searchable branch panel returns above"),
            // The provider's logins, each with its headroom or its state:
            // the numbers beside the choice are what make a router
            // unnecessary (`docs/accounts.md` §7).
            Picker::Account => self.account_rows(cx),
            Picker::Access => ginka_protocol::AccessMode::ALL
                .into_iter()
                .map(|mode| {
                    let chosen = self.chosen_access.unwrap_or_default() == mode;
                    self.picker_row(
                        SharedString::from(format!("access-option:{}", mode.as_str())),
                        access_label(mode),
                        Some(access_note(mode)),
                        chosen,
                        cx.listener(move |this, _, _, cx| {
                            this.chosen_access = Some(mode);
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
            Picker::Model => unreachable!("the searchable model panel returns above"),
            Picker::ReasoningEffort => {
                let mut rows = vec![self.picker_row(
                    "effort-option:default",
                    rust_i18n::t!("composer.option.provider_default").to_string(),
                    None,
                    self.model_options_to_start().0.is_none(),
                    cx.listener(|this, _, _, cx| {
                        let tier = this.model_options_to_start().1;
                        if !this.starting_new_session() {
                            this.update_existing_session_options(
                                this.model_to_start(),
                                None,
                                tier,
                                cx,
                            );
                            this.picker = None;
                            cx.notify();
                            return;
                        }
                        this.chosen_reasoning_effort = None;
                        this.chosen_service_tier = tier;
                        this.remember_current_model_options();
                        this.picker = None;
                        cx.notify();
                    }),
                    cx,
                )];
                if let Some(model) = self.selected_model() {
                    rows.extend(model.reasoning_efforts.into_iter().map(|option| {
                        let picked = option.id.clone();
                        let chosen =
                            self.model_options_to_start().0.as_deref() == Some(option.id.as_str());
                        self.picker_row(
                            SharedString::from(format!("effort-option:{}", option.id)),
                            option.label,
                            None,
                            chosen,
                            cx.listener(move |this, _, _, cx| {
                                let tier = this.model_options_to_start().1;
                                if !this.starting_new_session() {
                                    this.update_existing_session_options(
                                        this.model_to_start(),
                                        Some(picked.clone()),
                                        tier,
                                        cx,
                                    );
                                    this.picker = None;
                                    cx.notify();
                                    return;
                                }
                                this.chosen_reasoning_effort = Some(picked.clone());
                                this.chosen_service_tier = tier;
                                this.remember_current_model_options();
                                this.picker = None;
                                cx.notify();
                            }),
                            cx,
                        )
                    }));
                }
                rows
            }
            Picker::ServiceTier => {
                let mut rows = vec![self.picker_row(
                    "tier-option:default",
                    rust_i18n::t!("composer.option.provider_default").to_string(),
                    None,
                    self.model_options_to_start().1.is_none(),
                    cx.listener(|this, _, _, cx| {
                        let effort = this.model_options_to_start().0;
                        if !this.starting_new_session() {
                            this.update_existing_session_options(
                                this.model_to_start(),
                                effort,
                                None,
                                cx,
                            );
                            this.picker = None;
                            cx.notify();
                            return;
                        }
                        this.chosen_reasoning_effort = effort;
                        this.chosen_service_tier = None;
                        this.remember_current_model_options();
                        this.picker = None;
                        cx.notify();
                    }),
                    cx,
                )];
                if let Some(model) = self.selected_model() {
                    rows.extend(model.service_tiers.into_iter().map(|option| {
                        let picked = option.id.clone();
                        let chosen =
                            self.model_options_to_start().1.as_deref() == Some(option.id.as_str());
                        self.picker_row(
                            SharedString::from(format!("tier-option:{}", option.id)),
                            option.label,
                            None,
                            chosen,
                            cx.listener(move |this, _, _, cx| {
                                let effort = this.model_options_to_start().0;
                                if !this.starting_new_session() {
                                    this.update_existing_session_options(
                                        this.model_to_start(),
                                        effort,
                                        Some(picked.clone()),
                                        cx,
                                    );
                                    this.picker = None;
                                    cx.notify();
                                    return;
                                }
                                this.chosen_reasoning_effort = effort;
                                this.chosen_service_tier = Some(picked.clone());
                                this.remember_current_model_options();
                                this.picker = None;
                                cx.notify();
                            }),
                            cx,
                        )
                    }));
                }
                rows
            }
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

    /// Searchable model catalogue, grouped by the CLI that advertised it.
    ///
    /// Providers are switchable only before a conversation starts. Once a
    /// vendor session exists, changing its model remains possible when that
    /// driver supports it, but moving the conversation is the explicit
    /// handoff flow rather than a surprising side effect of this popover.
    fn model_picker_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let mut providers: Vec<AgentStatus> = ginka_ui::models::available_providers(&self.agents)
            .into_iter()
            .cloned()
            .collect();
        if !self.starting_new_session() {
            let current = self.agent_to_start()?;
            providers.retain(|provider| provider.id == current);
        }
        let active_id = self
            .agent_to_start()
            .filter(|id| providers.iter().any(|provider| provider.id == *id))
            .or_else(|| providers.first().map(|provider| provider.id.clone()))?;
        let active = providers.iter().find(|provider| provider.id == active_id)?;
        let models = ginka_ui::models::matching_models(active, &self.model_filter);
        let active_display_name = active.display_name.clone();

        let provider_tabs = providers.into_iter().map(|provider| {
            let id = provider.id.clone();
            let label = provider.display_name.clone();
            let initial = label.chars().next().unwrap_or('?').to_string();
            let selected = id == active_id;
            div()
                .id(SharedString::from(format!("model-provider:{id}")))
                .size(px(38.))
                .rounded(px(tokens.radius.control()))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .font_medium()
                .text_color(if selected {
                    tokens.colors().text_primary
                } else {
                    tokens.colors().text_muted
                })
                .when(selected, |this| this.bg(tokens.colors().row_active()))
                .hover(|this| this.bg(tokens.colors().row_hover()))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.chosen_agent = Some(id.clone());
                    this.chosen_model = None;
                    this.chosen_reasoning_effort = None;
                    this.chosen_service_tier = None;
                    this.chosen_account = None;
                    this.model_filter.clear();
                    this.model_query
                        .update(cx, |query, cx| query.set_value("", window, cx));
                    this.sync_footer(cx);
                    cx.notify();
                }))
                .child(initial)
        });

        let default_provider = active_id.clone();
        let mut rows = vec![self.picker_row(
            "model-option:default",
            rust_i18n::t!("composer.option.provider_default").to_string(),
            Some(active_display_name),
            self.model_to_start().is_none(),
            cx.listener(move |this, _, _, cx| {
                if this.starting_new_session() {
                    this.chosen_agent = Some(default_provider.clone());
                }
                this.choose_model(None, cx);
            }),
            cx,
        )];
        rows.extend(models.into_iter().map(|model| {
            let provider = active_id.clone();
            let picked = model.id.clone();
            let chosen = self.model_to_start().as_deref() == Some(model.id.as_str());
            let note = (!model.reasoning_efforts.is_empty()).then(|| {
                model
                    .reasoning_efforts
                    .iter()
                    .map(|option| option.label.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ")
            });
            self.picker_row(
                SharedString::from(format!("model-option:{}:{}", provider, model.id)),
                model.label,
                note,
                chosen,
                cx.listener(move |this, _, _, cx| {
                    if this.starting_new_session() {
                        this.chosen_agent = Some(provider.clone());
                        this.chosen_account = None;
                    }
                    this.choose_model(Some(picked.clone()), cx);
                }),
                cx,
            )
        }));
        if rows.len() == 1 && !self.model_filter.trim().is_empty() {
            rows.push(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(12.))
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("composer.model.empty").to_string())
                    .into_any_element(),
            );
        }

        Some(
            h_flex()
                .w_full()
                .h(px(360.))
                .rounded(px(tokens.radius.card))
                .bg(tokens.colors().popover())
                .border_1()
                .border_color(tokens.colors().border_strong)
                .shadow_lg()
                .overflow_hidden()
                .child(
                    v_flex()
                        .h_full()
                        .w(px(58.))
                        .p_2()
                        .gap_2()
                        .items_center()
                        .border_r_1()
                        .border_color(tokens.colors().border_subtle)
                        .children(provider_tabs),
                )
                .child(
                    v_flex()
                        .h_full()
                        .flex_1()
                        .child(
                            div()
                                .p_2()
                                .border_b_1()
                                .border_color(tokens.colors().border_subtle)
                                .child(Input::new(&self.model_query)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .p_1()
                                .gap_0p5()
                                .overflow_y_scrollbar()
                                .children(rows),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Open the branch picker and read git's current branch ownership.
    fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker == Some(Picker::Branch) {
            self.picker = None;
            cx.notify();
            return;
        }
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        self.picker = Some(Picker::Branch);
        self.branch_filter.clear();
        self.branches.clear();
        self.branch_error = None;
        self.branch_busy = true;
        self.branch_query
            .update(cx, |query, cx| query.set_value("", window, cx));
        self.branch_query
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();

        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { link.branches(&workspace).await })
                .await;
            this.update(cx, |this, cx| {
                this.branch_busy = false;
                match result {
                    Ok(branches) => this.branches = branches,
                    Err(error) => this.branch_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Take Return in the branch search as create or the best selectable match.
    fn take_branch_choice(&mut self, cx: &mut Context<Self>) {
        let Some(submission) =
            ginka_ui::branches::branch_submission(&self.branches, &self.branch_filter)
        else {
            return;
        };
        match submission {
            ginka_ui::branches::BranchSubmission::KeepCurrent => {
                self.picker = None;
                cx.notify();
            }
            ginka_ui::branches::BranchSubmission::Switch(branch) => {
                self.choose_branch(branch, false, cx)
            }
            ginka_ui::branches::BranchSubmission::Create(branch) => {
                self.choose_branch(branch, true, cx)
            }
        }
    }

    /// Switch or create the branch named by the picker.
    fn choose_branch(&mut self, branch: String, create: bool, cx: &mut Context<Self>) {
        if self.branch_busy {
            return;
        }
        let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) else {
            return;
        };
        self.branch_busy = true;
        self.branch_error = None;
        cx.notify();

        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { link.checkout_branch(&workspace, branch, create).await },
                )
                .await;
            this.update(cx, |this, cx| {
                this.branch_busy = false;
                match result {
                    Ok(()) => {
                        this.picker = None;
                        this.branch_filter.clear();
                    }
                    Err(error) => this.branch_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Searchable local branches plus an inline create action.
    fn branch_picker_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let options = ginka_ui::branches::matching_branches(&self.branches, &self.branch_filter);
        let create = ginka_ui::branches::new_branch_candidate(&self.branches, &self.branch_filter);
        let mut rows: Vec<AnyElement> = Vec::new();

        for option in options {
            let name = option.branch.name.clone();
            match option.availability {
                ginka_ui::branches::BranchAvailability::Current => {
                    rows.push(self.picker_row(
                        SharedString::from(format!("branch-option:{name}")),
                        name,
                        Some(rust_i18n::t!("composer.branch.current").to_string()),
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.picker = None;
                            cx.notify();
                        }),
                        cx,
                    ));
                }
                ginka_ui::branches::BranchAvailability::Available => {
                    let picked = name.clone();
                    rows.push(self.picker_row(
                        SharedString::from(format!("branch-option:{name}")),
                        name,
                        None,
                        false,
                        cx.listener(move |this, _, _, cx| {
                            this.choose_branch(picked.clone(), false, cx)
                        }),
                        cx,
                    ));
                }
                ginka_ui::branches::BranchAvailability::CheckedOutAt(path) => {
                    rows.push(
                        h_flex()
                            .id(SharedString::from(format!("branch-option:{name}")))
                            .w_full()
                            .h(px(30.))
                            .px(px(9.))
                            .gap(px(8.))
                            .items_center()
                            .rounded(px(tokens.radius.row))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(13.))
                                    .text_color(tokens.colors().text_muted)
                                    .truncate()
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(tokens.colors().text_muted)
                                    .truncate()
                                    .child(
                                        rust_i18n::t!(
                                            "composer.branch.checked_out",
                                            path = path.display().to_string()
                                        )
                                        .to_string(),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
            }
        }

        if let Some(branch) = create {
            let picked = branch.clone();
            rows.push(self.picker_row(
                SharedString::from("branch-option:create"),
                rust_i18n::t!("composer.branch.create", branch = branch).to_string(),
                None,
                false,
                cx.listener(move |this, _, _, cx| this.choose_branch(picked.clone(), true, cx)),
                cx,
            ));
        }

        if rows.is_empty() && self.branch_busy {
            rows.push(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(12.))
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("composer.branch.loading").to_string())
                    .into_any_element(),
            );
        } else if rows.is_empty() && self.branch_error.is_none() {
            rows.push(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(12.))
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("composer.branch.empty").to_string())
                    .into_any_element(),
            );
        }

        Some(
            v_flex()
                .w_full()
                .max_h(px(360.))
                .rounded(px(tokens.radius.card))
                .bg(tokens.colors().popover())
                .border_1()
                .border_color(tokens.colors().border_strong)
                .shadow_lg()
                .overflow_hidden()
                .child(
                    div()
                        .p_2()
                        .border_b_1()
                        .border_color(tokens.colors().border_subtle)
                        .child(Input::new(&self.branch_query)),
                )
                .children(self.branch_error.clone().map(|error| {
                    div()
                        .px_3()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(tokens.colors().status_error)
                        .child(error)
                }))
                .child(
                    v_flex()
                        .p_1()
                        .gap_0p5()
                        .overflow_y_scrollbar()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    /// Apply a model choice and preserve the most recent valid effort/tier.
    fn choose_model(&mut self, picked: Option<String>, cx: &mut Context<Self>) {
        if !self.starting_new_session() {
            let recent = picked
                .as_ref()
                .and_then(|picked| self.models().into_iter().find(|model| model.id == *picked))
                .and_then(|model| {
                    self.agent_to_start()
                        .map(|agent| self.settings.recent_model_options(&agent, &model))
                })
                .unwrap_or_default();
            self.update_existing_session_options(
                picked,
                recent.reasoning_effort,
                recent.service_tier,
                cx,
            );
        } else {
            self.chosen_model = picked.clone();
            self.chosen_reasoning_effort = None;
            self.chosen_service_tier = None;
            if let Some(agent) = self.agent_to_start() {
                if let Some(picked) = picked {
                    self.settings.remember_model(agent.clone(), picked);
                    if let Some(model) = self.selected_model() {
                        let recent = self.settings.recent_model_options(&agent, &model);
                        self.chosen_reasoning_effort = recent.reasoning_effort;
                        self.chosen_service_tier = recent.service_tier;
                    }
                } else {
                    self.settings.forget_model(&agent);
                }
                self.persist();
            }
        }
        self.picker = None;
        cx.notify();
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
    fn models(&self) -> Vec<ProviderModel> {
        self.agent_to_start()
            .and_then(|id| {
                self.agents
                    .iter()
                    .find(|agent| agent.id == id)
                    .map(|agent| agent.models.clone())
            })
            .unwrap_or_default()
    }

    /// The explicit model, then the last valid choice for this provider.
    fn model_to_start(&self) -> Option<String> {
        if !self.starting_new_session() {
            return self.session.as_ref().and_then(|row| row.model.clone());
        }
        if let Some(model) = &self.chosen_model {
            return Some(model.clone());
        }
        let agent = self.agent_to_start()?;
        let models = self.models();
        self.settings
            .recent_model(&agent, &models)
            .map(str::to_string)
    }

    /// The catalogue entry for the model that will run next.
    fn selected_model(&self) -> Option<ProviderModel> {
        let chosen = self.model_to_start()?;
        self.models().into_iter().find(|model| model.id == chosen)
    }

    /// Valid effort and tier choices for the next turn.
    fn model_options_to_start(&self) -> (Option<String>, Option<String>) {
        let Some(model) = self.selected_model() else {
            return (None, None);
        };
        if !self.starting_new_session() {
            let Some(row) = self.session.as_ref() else {
                return (None, None);
            };
            return (
                row.reasoning_effort
                    .as_ref()
                    .filter(|effort| model.supports_reasoning_effort(effort))
                    .cloned(),
                row.service_tier
                    .as_ref()
                    .filter(|tier| model.supports_service_tier(tier))
                    .cloned(),
            );
        }
        let Some(agent) = self.agent_to_start() else {
            return (None, None);
        };
        let recent = self.settings.recent_model_options(&agent, &model);
        let effort = self
            .chosen_reasoning_effort
            .as_ref()
            .filter(|effort| model.supports_reasoning_effort(effort))
            .cloned()
            .or(recent.reasoning_effort);
        let tier = self
            .chosen_service_tier
            .as_ref()
            .filter(|tier| model.supports_service_tier(tier))
            .cloned()
            .or(recent.service_tier);
        (effort, tier)
    }

    /// Persist the currently selected non-default options for this model.
    fn remember_current_model_options(&mut self) {
        let Some(agent) = self.agent_to_start() else {
            return;
        };
        let Some(model) = self.selected_model() else {
            return;
        };
        self.settings.remember_model_options(
            &agent,
            &model.id,
            self.chosen_reasoning_effort.clone(),
            self.chosen_service_tier.clone(),
        );
        self.persist();
    }

    /// Apply a complete provider option set to the selected conversation.
    fn update_existing_session_options(
        &mut self,
        model: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.session.as_mut() else {
            return;
        };
        let Some(session) = row.session.clone() else {
            return;
        };
        let previous = (
            row.model.clone(),
            row.reasoning_effort.clone(),
            row.service_tier.clone(),
        );
        let requested = (model, reasoning_effort, service_tier);
        row.model = requested.0.clone();
        row.reasoning_effort = requested.1.clone();
        row.service_tier = requested.2.clone();
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let sending = requested.clone();
            let original_session = session.clone();
            let result = cx
                .background_spawn(async move {
                    link.update_session_options(session, sending.0, sending.1, sending.2)
                        .await
                })
                .await;
            this.update(cx, |this, cx| {
                let mut accepted = None;
                if let Some(row) = this.session.as_mut() {
                    if row.session.as_ref() != Some(&original_session) {
                        return;
                    }
                    let current = (
                        row.model.clone(),
                        row.reasoning_effort.clone(),
                        row.service_tier.clone(),
                    );
                    // A newer click already won. Its response will settle its
                    // own optimistic state; this older one must not overwrite
                    // or roll it back when requests finish out of order.
                    if current != requested {
                        return;
                    }
                    match &result {
                        Some((session, _)) => {
                            row.session = Some(session.id.clone());
                            row.model = session.model.clone();
                            row.reasoning_effort = session.reasoning_effort.clone();
                            row.service_tier = session.service_tier.clone();
                            accepted = Some((
                                session.agent.clone(),
                                session.model.clone(),
                                session.reasoning_effort.clone(),
                                session.service_tier.clone(),
                            ));
                        }
                        _ => {
                            row.model = previous.0.clone();
                            row.reasoning_effort = previous.1.clone();
                            row.service_tier = previous.2.clone();
                        }
                    }
                }
                if let Some((agent, model, effort, tier)) = accepted {
                    match model {
                        Some(model) => {
                            this.settings.remember_model(&agent, &model);
                            this.settings
                                .remember_model_options(&agent, &model, effort, tier);
                        }
                        None => this.settings.forget_model(&agent),
                    }
                    this.persist();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        if self.editing_queued_message.is_none() {
            let link = self.link.clone();
            let saving = text.clone();
            let for_draft = workspace.clone();
            cx.background_spawn(async move { link.save_draft(&for_draft, saving).await })
                .detach();
        }

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

    /// Aim the chat at a project — or at none — from the composer.
    ///
    /// It starts a new conversation rather than moving the one on screen: an
    /// answer belongs to the worktree it was produced in, and a chat that
    /// changed project under a finished transcript would be claiming otherwise.
    fn choose_project(
        &mut self,
        project: Option<ProjectName>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker = None;
        self.start_new_chat(project, window, cx);
    }

    /// Open a picker, or close it if it is the one already open.
    fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.picker = (self.picker != Some(picker)).then_some(picker);
        cx.notify();
    }

    /// The access chip: what the next session's agent may touch, and the way
    /// to change it. Only while a new session is what the composer would
    /// start — a mode is fixed once a conversation has begun (§3.3 N2), and a
    /// chip that could not be changed would read as one that could.
    fn access_chip_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let starting_fresh = match self.session.as_ref() {
            Some(row) => row.session.is_none() || self.start_fresh,
            None => true,
        };
        if !starting_fresh {
            return None;
        }
        let tokens = Tokens::global(cx);
        let mode = self.chosen_access.unwrap_or_default();
        Some(
            h_flex()
                .id("access-chip")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().row_hover())
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .tooltip(|window, cx| {
                    Tooltip::new(rust_i18n::t!("composer.access.pick").to_string())
                        .build(window, cx)
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(Picker::Access, cx)))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(tokens.colors().text_secondary)
                        .child(access_label(mode)),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(12.))
                        .text_color(tokens.colors().text_muted),
                ),
        )
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

    /// The login the next prompt runs on.
    ///
    /// A conversation stays on the login it started on — the vendor's thread
    /// lives in that login's directory — so while one is being continued the
    /// answer is its account. A fresh chat takes what the reader picked, or
    /// the provider's persisted active account.
    fn account_to_start(&self) -> Option<&ginka_protocol::model::Account> {
        let provider = self.agent_to_start()?;
        let continuing = self
            .session
            .as_ref()
            .filter(|row| row.session.is_some() && !self.start_fresh)
            .and_then(|row| row.account.as_ref());
        ginka_ui::accounts::account_to_start(
            &self.accounts,
            &provider,
            continuing.or(self.chosen_account.as_ref()),
        )
    }

    /// What a login's row says beside its name: the tightest window and the
    /// reading's age, or that it is signed out, or that nothing is known.
    fn account_note(&self, account: &ginka_protocol::model::Account, now: i64) -> Option<String> {
        if account.signed_in == Some(false) {
            return Some(rust_i18n::t!("composer.agent.signed_out").to_string());
        }
        let snapshot = ginka_ui::accounts::snapshot_of(&self.plans, &account.id);
        match ginka_ui::accounts::headroom(snapshot, now) {
            Some(headroom) => {
                let mut parts = vec![headroom.summary()];
                if headroom.exhausted {
                    parts.push(rust_i18n::t!("composer.account.at_wall").to_string());
                }
                if !headroom.reset.is_empty() {
                    parts.push(headroom.reset.clone());
                }
                parts.push(
                    rust_i18n::t!(
                        "composer.account.age",
                        age = ginka_ui::workspace::relative_age(now, now - headroom.age)
                    )
                    .to_string(),
                );
                Some(parts.join(" · "))
            }
            None => Some(rust_i18n::t!("composer.account.no_reading").to_string()),
        }
    }

    /// Ask the daemon for a fresh reading of every login of the chosen
    /// provider whose reading is missing or older than the shortest window.
    ///
    /// On opening the picker rather than on a timer: a quota is cheap to
    /// observe from the traffic that spends it, and a request when the number
    /// is wanted is the one active refresh the design allows.
    fn refresh_stale_accounts(&self, cx: &mut Context<Self>) {
        let Some(provider) = self.agent_to_start() else {
            return;
        };
        let now = crate::daemon::now();
        let stale: Vec<ginka_protocol::AccountId> =
            ginka_ui::accounts::accounts_for(&self.accounts, &provider)
                .into_iter()
                .filter(|account| account.signed_in != Some(false))
                .filter(|account| {
                    ginka_ui::accounts::wants_refresh(
                        ginka_ui::accounts::snapshot_of(&self.plans, &account.id),
                        now,
                    )
                })
                .map(|account| account.id.clone())
                .collect();
        for account in stale {
            let link = self.link.clone();
            cx.background_spawn(async move {
                // The daemon pushes the reading; nothing to do with the
                // answer here.
                let _ = link.refresh_plan(&account).await;
            })
            .detach();
        }
    }

    /// The account chip, present only when the chosen agent has more than
    /// one login: a chip that could only restate the agent chip is noise.
    fn account_chip_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let provider = self.agent_to_start()?;
        if !ginka_ui::accounts::offers_choice(&self.accounts, &provider) {
            return None;
        }
        let account = self.account_to_start()?;
        let tokens = Tokens::global(cx);
        let now = crate::daemon::now();
        let headroom = ginka_ui::accounts::headroom(
            ginka_ui::accounts::snapshot_of(&self.plans, &account.id),
            now,
        );
        let (note, colour) = match (&headroom, account.signed_in) {
            (_, Some(false)) => (
                Some(rust_i18n::t!("composer.agent.signed_out").to_string()),
                tokens.colors().status_attention,
            ),
            (Some(headroom), _) if headroom.exhausted => (
                Some(format!(
                    "{} · {}",
                    headroom.summary(),
                    rust_i18n::t!("composer.account.at_wall")
                )),
                tokens.colors().status_attention,
            ),
            (Some(headroom), _) => (Some(headroom.summary()), tokens.colors().text_secondary),
            (None, _) => (None, tokens.colors().text_secondary),
        };
        let label = account.label.clone();

        Some(
            h_flex()
                .id("account-chip")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().row_hover())
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .tooltip(|window, cx| {
                    Tooltip::new(rust_i18n::t!("composer.account.pick").to_string())
                        .build(window, cx)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.picker != Some(Picker::Account) {
                        this.refresh_stale_accounts(cx);
                    }
                    this.toggle_picker(Picker::Account, cx)
                }))
                .child(div().text_size(px(12.)).text_color(colour).child(label))
                .children(note.map(|note| {
                    div()
                        .text_size(px(12.))
                        .text_color(colour)
                        .child(format!("· {note}"))
                }))
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(12.))
                        .text_color(tokens.colors().text_muted),
                ),
        )
    }

    /// The active login's tightest usage window, even when there is only one
    /// login and therefore no account picker chip.
    ///
    /// Turn events update this passively. Clicking asks the daemon for an
    /// explicit fresh reading; there is deliberately no background quota
    /// polling (`docs/accounts.md` §7).
    fn usage_chip_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let account = self.account_to_start()?;
        if account.signed_in == Some(false) {
            return None;
        }
        let tokens = Tokens::global(cx);
        let headroom = ginka_ui::accounts::headroom(
            ginka_ui::accounts::snapshot_of(&self.plans, &account.id),
            crate::daemon::now(),
        );
        let token_count = ginka_ui::reports::session_token_count(&self.transcript.usage());
        let context = self
            .transcript
            .context_usage()
            .as_ref()
            .map(ginka_ui::reports::context_window_summary);
        let exhausted = headroom.as_ref().is_some_and(|headroom| headroom.exhausted);
        let headroom_label = headroom.as_ref().map(|headroom| {
            if headroom.exhausted {
                format!(
                    "{} · {}",
                    headroom.summary(),
                    rust_i18n::t!("composer.account.at_wall")
                )
            } else {
                headroom.summary()
            }
        });
        let mut labels = Vec::new();
        if let Some(context) = context {
            labels.push(rust_i18n::t!("composer.usage.context", summary = context).to_string());
        }
        if let Some(count) = token_count {
            labels.push(rust_i18n::t!("composer.usage.tokens", count = count).to_string());
        }
        if let Some(headroom) = headroom_label {
            labels.push(headroom);
        }
        let label = if labels.is_empty() {
            rust_i18n::t!("composer.usage.unknown").to_string()
        } else {
            labels.join(" · ")
        };
        let account = account.id.clone();

        Some(
            h_flex()
                .id("usage-chip")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().row_hover())
                .text_size(px(12.))
                .text_color(if exhausted {
                    tokens.colors().status_attention
                } else {
                    tokens.colors().text_secondary
                })
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .tooltip(|window, cx| {
                    Tooltip::new(rust_i18n::t!("composer.usage.refresh").to_string())
                        .build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let link = this.link.clone();
                    let account = account.clone();
                    cx.background_spawn(async move {
                        let _ = link.refresh_plan(&account).await;
                    })
                    .detach();
                }))
                .child(label),
        )
    }

    /// Manual context compaction, only when the provider explicitly reported
    /// support and no turn can be interrupted by it.
    fn compact_context_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let session = self.transcript_of.clone()?;
        let busy = self.is_working()
            || matches!(
                self.session_state,
                Some(SessionState::Starting | SessionState::Running | SessionState::AwaitingInput)
            );
        if !ginka_ui::reports::can_compact_context(self.transcript.context_usage().as_ref(), busy) {
            return None;
        }

        Some(
            Button::new("compact-context")
                .ghost()
                .compact()
                .tooltip(rust_i18n::t!("composer.usage.compact_tooltip").to_string())
                .label(rust_i18n::t!("composer.usage.compact").to_string())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let link = this.link.clone();
                    let session = session.clone();
                    cx.background_spawn(async move {
                        let _ = link.compact_session(&session).await;
                    })
                    .detach();
                })),
        )
    }

    /// The agent picker's rows: every agent this machine has, then a way to
    /// add a login for the chosen one, so the first second login is
    /// reachable before there is an account chip to open.
    fn agent_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = self
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
                        // The model and the login belonged to the agent that
                        // was chosen before; they mean nothing to the new
                        // one.
                        this.chosen_model = None;
                        this.chosen_reasoning_effort = None;
                        this.chosen_service_tier = None;
                        this.chosen_account = None;
                        this.picker = None;
                        this.sync_footer(cx);
                        cx.notify();
                    }),
                    cx,
                )
            })
            .collect();
        rows.extend(self.add_login_row(cx));
        rows
    }

    /// The account picker's rows: the provider's logins, each with its
    /// headroom or its state — the numbers beside the choice are what make a
    /// router unnecessary (`docs/accounts.md` §7) — and a way to add one.
    fn account_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let provider = self.agent_to_start().unwrap_or_default();
        let current = self.account_to_start().map(|account| account.id.clone());
        let now = crate::daemon::now();
        let mut rows: Vec<AnyElement> = ginka_ui::accounts::accounts_for(&self.accounts, &provider)
            .into_iter()
            .map(|account| {
                let id = account.id.clone();
                let signed_out = account.signed_in == Some(false);
                let note = self.account_note(account, now);
                self.picker_row(
                    SharedString::from(format!("account-option:{}", account.id.0)),
                    account.label.clone(),
                    note,
                    current.as_ref() == Some(&account.id),
                    cx.listener(move |this, _, window, cx| {
                        this.chosen_account = Some(id.clone());
                        this.picker = None;
                        let link = this.link.clone();
                        let selected = id.clone();
                        cx.background_spawn(async move {
                            let _ = link.select_account(&selected).await;
                        })
                        .detach();
                        // Choosing a login that is signed out is asking to
                        // sign in: the vendor's own command opens in the
                        // dock, pointed at the login's directory.
                        if signed_out {
                            this.sign_in(id.clone(), window, cx);
                        }
                        this.sync_footer(cx);
                        cx.notify();
                    }),
                    cx,
                )
            })
            .collect();
        rows.extend(self.add_login_row(cx));
        rows
    }

    /// The row that opens the add-login dialog for the chosen agent.
    fn add_login_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let provider = self.agent_to_start()?;
        let display = self
            .agents
            .iter()
            .find(|agent| agent.id == provider)
            .map(|agent| agent.display_name.clone())
            .unwrap_or_else(|| provider.clone());
        Some(self.picker_row(
            SharedString::from("account-option:add"),
            rust_i18n::t!("composer.account.add", agent = display).to_string(),
            None,
            false,
            cx.listener(move |this, _, window, cx| {
                this.picker = None;
                this.open_add_account(provider.clone(), window, cx);
            }),
            cx,
        ))
    }

    /// Tell the sidebar's footer which login the next prompt runs on and
    /// how much of its window is left.
    fn sync_footer(&self, cx: &mut Context<Self>) {
        let now = crate::daemon::now();
        let account = self.account_to_start().map(|account| {
            let display = self
                .agents
                .iter()
                .find(|agent| agent.id == account.provider.as_str())
                .map(|agent| agent.display_name.clone())
                .unwrap_or_else(|| account.provider.to_string());
            let label = if account.is_default {
                display
            } else {
                format!("{} · {display}", account.label)
            };
            (label, self.account_note(account, now))
        });
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_account(account, cx));
    }

    /// Open the dialog that adds a login for `provider`.
    fn open_add_account(&mut self, provider: String, window: &mut Window, cx: &mut Context<Self>) {
        let id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("composer.account.add.id").to_string())
        });
        let label = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("composer.account.add.label").to_string())
        });
        id.read(cx).focus_handle(cx).focus(window, cx);
        self.add_account = Some(AddAccount {
            provider,
            id,
            label,
            error: None,
            busy: false,
        });
        cx.notify();
    }

    /// Send the dialog's login to the daemon, and close it when the daemon
    /// took it — or show what the daemon said when it did not.
    fn submit_add_account(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.add_account.as_mut() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let typed_id = dialog.id.read(cx).value().to_string();
        let typed_label = dialog.label.read(cx).value().to_string();
        let Some(provider) = ginka_protocol::ProviderKind::parse(&dialog.provider) else {
            dialog.error = Some(rust_i18n::t!("composer.account.add.no_provider").to_string());
            cx.notify();
            return;
        };
        dialog.busy = true;
        dialog.error = None;
        cx.notify();
        let link = self.link.clone();
        cx.spawn(async move |this, cx| {
            let id = ginka_protocol::AccountId(typed_id.trim().to_string());
            let label = if typed_label.trim().is_empty() {
                id.0.clone()
            } else {
                typed_label.trim().to_string()
            };
            let added = cx
                .background_spawn(async move {
                    let added = link.add_account(id, provider, label).await;
                    if let Ok(account) = &added {
                        // A newly added login is the one the reader is about
                        // to sign into, so future conversations should adopt it.
                        let _ = link.select_account(&account.id).await;
                    }
                    added
                })
                .await;
            this.update(cx, |this, cx| {
                match added {
                    Ok(account) => {
                        this.add_account = None;
                        this.chosen_account = Some(account.id.clone());
                        // The list is re-read on the daemon's push; adopting
                        // the record now is what lets the chip say it before
                        // the push lands.
                        this.accounts.push(account);
                        this.sync_footer(cx);
                    }
                    Err(message) => {
                        if let Some(dialog) = this.add_account.as_mut() {
                            dialog.busy = false;
                            dialog.error = Some(message);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Escape closes the dialog and Return submits it, from either field.
    fn add_account_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                self.add_account = None;
                cx.notify();
            }
            "enter" => self.submit_add_account(cx),
            _ => {}
        }
    }

    /// Escape closes the project dialog and Return creates it when complete.
    fn add_project_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                self.add_project = None;
                cx.notify();
            }
            "enter" => self.submit_add_project(window, cx),
            _ => {}
        }
    }

    /// Image annotation stays in application chrome and produces one safe,
    /// self-contained SVG that follows the ordinary attachment path.
    fn image_markup_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let markup = self.image_markup.as_ref()?;
        let tokens = Tokens::global(cx).clone();
        let preview_url = markup.preview_url.clone();
        let selected_tool = markup.tool;
        let text_input = markup.text.clone();
        let shapes = markup.document.shapes().to_vec();
        let active = markup.document.active().cloned();
        let shell = cx.entity();
        let has_marks = !shapes.is_empty();
        let busy = self.attachment_busy;
        let tool_buttons = ginka_ui::markup::MarkupTool::ALL
            .iter()
            .copied()
            .map(|tool| {
                Button::new(SharedString::from(format!("markup-tool-{}", tool.label())))
                    .compact()
                    .when(tool != selected_tool, |button| button.ghost())
                    .label(markup_tool_label(tool))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(markup) = this.image_markup.as_mut() {
                            markup.tool = tool;
                            markup.drawing = false;
                        }
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(tokens.colors().bg_window.opacity(0.82))
                .child(
                    v_flex()
                        .id("image-markup-dialog")
                        .w(px(920.))
                        .max_w_full()
                        .p_4()
                        .gap_3()
                        .rounded(px(tokens.radius.card))
                        .bg(tokens.colors().bg_raised)
                        .border_1()
                        .border_color(tokens.colors().border_strong)
                        .shadow_lg()
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_lg()
                                        .font_semibold()
                                        .child(rust_i18n::t!("composer.markup.title").to_string()),
                                )
                                .child(
                                    Button::new("close-image-markup")
                                        .ghost()
                                        .compact()
                                        .icon(IconName::Close)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.image_markup = None;
                                            this.surfaces.update(cx, |surfaces, cx| {
                                                surfaces.suspend_browser(false, cx)
                                            });
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_1()
                                .children(tool_buttons)
                                .when(selected_tool == ginka_ui::markup::MarkupTool::Text, |row| {
                                    row.child(div().ml_2().flex_1().child(Input::new(&text_input)))
                                })
                                .child(div().flex_1())
                                .child(
                                    Button::new("undo-image-markup")
                                        .ghost()
                                        .compact()
                                        .label(rust_i18n::t!("composer.markup.undo").to_string())
                                        .disabled(!has_marks)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(markup) = this.image_markup.as_mut() {
                                                markup.document.undo();
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("clear-image-markup")
                                        .ghost()
                                        .compact()
                                        .label(rust_i18n::t!("composer.markup.clear").to_string())
                                        .disabled(!has_marks)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(markup) = this.image_markup.as_mut() {
                                                markup.document.clear();
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .h(px(500.))
                                .overflow_hidden()
                                .rounded(px(tokens.radius.row))
                                .bg(tokens.colors().bg_window)
                                .cursor_crosshair()
                                .child(
                                    img(SharedString::from(preview_url))
                                        .absolute()
                                        .size_full()
                                        .object_fit(ObjectFit::Fill),
                                )
                                .child(
                                    canvas(
                                        move |_, _, _| {},
                                        move |bounds, _, window, _| {
                                            for shape in &shapes {
                                                paint_markup_shape(shape, bounds, window);
                                            }
                                            if let Some(shape) = &active {
                                                paint_markup_shape(shape, bounds, window);
                                            }
                                        },
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                                .on_prepaint(move |bounds, _, cx| {
                                    shell.update(cx, |this, _| {
                                        if let Some(markup) = this.image_markup.as_mut() {
                                            markup.canvas_bounds = bounds;
                                        }
                                    });
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                        this.begin_markup(event, cx)
                                    }),
                                )
                                .on_mouse_move(cx.listener(
                                    |this, event: &MouseMoveEvent, _, cx| {
                                        this.extend_markup(event, cx)
                                    },
                                ))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, event: &MouseUpEvent, _, cx| {
                                        this.finish_markup(event, cx)
                                    }),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-image-markup")
                                        .ghost()
                                        .label(rust_i18n::t!("composer.markup.cancel").to_string())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.image_markup = None;
                                            this.surfaces.update(cx, |surfaces, cx| {
                                                surfaces.suspend_browser(false, cx)
                                            });
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("attach-image-markup")
                                        .primary()
                                        .label(rust_i18n::t!("composer.markup.attach").to_string())
                                        .disabled(!has_marks || busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.submit_image_markup(cx)
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Project name and source-folder selection, drawn as one modal workflow.
    fn add_project_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.add_project.as_ref()?;
        let tokens = Tokens::global(cx).clone();
        let source = dialog
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| rust_i18n::t!("project.add.source.empty").to_string());
        let ready = !dialog.busy
            && validate_project_draft(dialog.name.read(cx).value().as_ref(), dialog.path.clone())
                .is_ok();

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(tokens.colors().bg_window.opacity(0.72))
                .child(
                    v_flex()
                        .id("add-project-dialog")
                        .w(px(620.))
                        .p_5()
                        .gap_4()
                        .rounded(px(tokens.radius.card))
                        .bg(tokens.colors().bg_raised)
                        .border_1()
                        .border_color(tokens.colors().border_strong)
                        .shadow_lg()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            this.add_project_key(event, window, cx)
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xl()
                                        .font_semibold()
                                        .text_color(tokens.colors().text_primary)
                                        .child(rust_i18n::t!("project.add.title").to_string()),
                                )
                                .child(
                                    div()
                                        .id("add-project-close")
                                        .p_1()
                                        .rounded(px(tokens.radius.control()))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(tokens.colors().row_hover()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.add_project = None;
                                            cx.notify();
                                        }))
                                        .child(
                                            Icon::new(IconName::Close)
                                                .size_4()
                                                .text_color(tokens.colors().text_secondary),
                                        ),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .h(px(46.))
                                .px_3()
                                .gap_2()
                                .items_center()
                                .rounded(px(tokens.radius.control()))
                                .border_1()
                                .border_color(tokens.colors().border_strong)
                                .child(
                                    Icon::new(IconName::Folder)
                                        .size_4()
                                        .text_color(tokens.colors().text_secondary),
                                )
                                .child(div().flex_1().min_w_0().child(Input::new(&dialog.name))),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(tokens.colors().text_secondary)
                                .child(rust_i18n::t!("project.add.source.label").to_string()),
                        )
                        .child(
                            v_flex()
                                .id("choose-project-folder")
                                .w_full()
                                .h(px(118.))
                                .px_4()
                                .gap_2()
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens.radius.control()))
                                .border_1()
                                .border_color(tokens.colors().border_subtle)
                                .cursor_pointer()
                                .hover(|this| this.bg(tokens.colors().row_hover()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_project_folder(window, cx)
                                }))
                                .child(
                                    Icon::new(IconName::Folder)
                                        .size_5()
                                        .text_color(tokens.colors().text_muted),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(tokens.colors().text_primary)
                                        .child(source),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(tokens.colors().text_muted)
                                        .child(
                                            rust_i18n::t!("project.add.source.hint").to_string(),
                                        ),
                                ),
                        )
                        .children(dialog.error.clone().map(|error| {
                            div()
                                .text_xs()
                                .text_color(tokens.colors().status_error)
                                .child(error)
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("add-project-cancel")
                                        .px_3()
                                        .py_2()
                                        .rounded(px(tokens.radius.row))
                                        .text_sm()
                                        .text_color(tokens.colors().text_secondary)
                                        .cursor_pointer()
                                        .hover(|this| this.bg(tokens.colors().row_hover()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.add_project = None;
                                            cx.notify();
                                        }))
                                        .child(rust_i18n::t!("project.add.cancel").to_string()),
                                )
                                .child(
                                    div()
                                        .id("add-project-submit")
                                        .px_4()
                                        .py_2()
                                        .rounded(px(tokens.radius.row))
                                        .bg(tokens.colors().accent.opacity(if ready {
                                            0.7
                                        } else {
                                            0.2
                                        }))
                                        .text_sm()
                                        .text_color(if ready {
                                            tokens.colors().text_primary
                                        } else {
                                            tokens.colors().text_muted
                                        })
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_add_project(window, cx)
                                        }))
                                        .child(rust_i18n::t!("project.add.submit").to_string()),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The add-login dialog, drawn over the window like the palette.
    fn add_account_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.add_account.as_ref()?;
        let tokens = Tokens::global(cx).clone();
        let display = self
            .agents
            .iter()
            .find(|agent| agent.id == dialog.provider)
            .map(|agent| agent.display_name.clone())
            .unwrap_or_else(|| dialog.provider.clone());
        let busy = dialog.busy;

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
                        .id("add-account")
                        .mt(px(120.))
                        .w(px(460.))
                        .p_4()
                        .gap_3()
                        .rounded(px(tokens.radius.card))
                        .bg(tokens.colors().bg_raised)
                        .border_1()
                        .border_color(tokens.colors().border_strong)
                        .shadow_lg()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            this.add_account_key(event, cx)
                        }))
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(tokens.colors().text_primary)
                                .child(
                                    rust_i18n::t!("composer.account.add.title", agent = display)
                                        .to_string(),
                                ),
                        )
                        .child(Input::new(&dialog.id))
                        .child(Input::new(&dialog.label))
                        .child(
                            div()
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .child(rust_i18n::t!("composer.account.add.hint").to_string()),
                        )
                        .children(dialog.error.clone().map(|error| {
                            div()
                                .text_xs()
                                .text_color(tokens.colors().status_error)
                                .child(error)
                        }))
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("add-account-cancel")
                                        .px(px(9.))
                                        .py(px(5.))
                                        .rounded(px(tokens.radius.row))
                                        .text_size(px(12.))
                                        .text_color(tokens.colors().text_secondary)
                                        .cursor_pointer()
                                        .hover(|this| this.bg(tokens.colors().row_hover()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.add_account = None;
                                            cx.notify();
                                        }))
                                        .child(
                                            rust_i18n::t!("composer.account.add.cancel")
                                                .to_string(),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("add-account-submit")
                                        .px(px(9.))
                                        .py(px(5.))
                                        .rounded(px(tokens.radius.row))
                                        .bg(tokens.colors().accent.opacity(if busy {
                                            0.3
                                        } else {
                                            0.6
                                        }))
                                        .text_size(px(12.))
                                        .text_color(tokens.colors().text_primary)
                                        .cursor_pointer()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.submit_add_account(cx)
                                            }),
                                        )
                                        .child(
                                            rust_i18n::t!("composer.account.add.submit")
                                                .to_string(),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
        )
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
        let selected = self
            .model_to_start()
            .and_then(|chosen| models.iter().find(|model| model.id == chosen));
        let label = selected
            .map(|model| model.label.clone())
            .unwrap_or_else(|| rust_i18n::t!("composer.model.default").to_string());
        let effort = self.model_options_to_start().0;
        let effort_label = selected
            .and_then(|model| ginka_ui::models::effort_label(model, effort.as_deref()))
            .map(str::to_string);

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
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.picker == Some(Picker::Model) {
                        this.picker = None;
                    } else {
                        this.picker = Some(Picker::Model);
                        this.model_filter.clear();
                        this.model_query
                            .update(cx, |query, cx| query.set_value("", window, cx));
                        this.model_query.read(cx).focus_handle(cx).focus(window, cx);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(tokens.colors().text_secondary)
                        .child(label),
                )
                .children(effort_label.map(|effort| {
                    div()
                        .text_size(px(11.))
                        .text_color(tokens.colors().text_muted)
                        .child(effort)
                }))
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(12.))
                        .text_color(tokens.colors().text_muted),
                ),
        )
    }

    /// Whether composer option changes can still affect the next session.
    fn starting_new_session(&self) -> bool {
        self.start_fresh
            || self
                .session
                .as_ref()
                .and_then(|row| row.session.as_ref())
                .is_none()
    }

    /// Reasoning chip when the selected model advertises a choice.
    fn reasoning_effort_chip_button(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let model = self.selected_model()?;
        if model.reasoning_efforts.is_empty() {
            return None;
        }
        let tokens = Tokens::global(cx);
        let label = self
            .model_options_to_start()
            .0
            .and_then(|chosen| {
                model
                    .reasoning_efforts
                    .iter()
                    .find(|option| option.id == chosen)
                    .map(|option| option.label.clone())
            })
            .unwrap_or_else(|| rust_i18n::t!("composer.effort.default").to_string());
        Some(
            h_flex()
                .id("reasoning-effort-chip")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().row_hover())
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .on_click(
                    cx.listener(|this, _, _, cx| this.toggle_picker(Picker::ReasoningEffort, cx)),
                )
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

    /// Service-tier chip when the selected model advertises a choice.
    fn service_tier_chip_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let model = self.selected_model()?;
        if model.service_tiers.is_empty() {
            return None;
        }
        let tokens = Tokens::global(cx);
        let label = self
            .model_options_to_start()
            .1
            .and_then(|chosen| {
                model
                    .service_tiers
                    .iter()
                    .find(|option| option.id == chosen)
                    .map(|option| option.label.clone())
            })
            .unwrap_or_else(|| rust_i18n::t!("composer.tier.default").to_string());
        Some(
            h_flex()
                .id("service-tier-chip")
                .h(px(28.))
                .px(px(9.))
                .gap(px(6.))
                .items_center()
                .rounded(px(tokens.radius.row))
                .bg(tokens.colors().row_hover())
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_active()))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(Picker::ServiceTier, cx)))
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
        let chosen = self.agent_to_start();
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

    /// The hairline strip under the composer: where this runs on the left,
    /// which branch on the right. `docs/ui.md` §3.3.
    ///
    /// The project is a chip rather than a label because it is a choice: it is
    /// how a chat is aimed at a project without going to the sidebar, and how
    /// a project is registered from the middle of the window where the reader
    /// already is.
    fn context_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let label = self
            .project_label()
            .unwrap_or_else(|| rust_i18n::t!("composer.context.project").to_string().into());
        let chosen = self.project_label().is_some();
        let branch = self
            .session
            .as_ref()
            .map(|session| session.branch.clone())
            .filter(|branch| !branch.is_empty());
        let branch_picker = branch.is_some();
        let indexed = self.session.as_ref().map(|session| session.indexed);
        let index_label = if self.index_starting {
            rust_i18n::t!("composer.index.starting").to_string()
        } else if indexed == Some(true) {
            rust_i18n::t!("composer.index.ready").to_string()
        } else if self.index_error.is_some() {
            rust_i18n::t!("composer.index.failed").to_string()
        } else {
            rust_i18n::t!("composer.index.action").to_string()
        };
        let index_colour = if self.index_error.is_some() {
            tokens.colors().status_error
        } else if indexed == Some(true) {
            tokens.colors().status_done
        } else {
            tokens.colors().text_muted
        };

        h_flex()
            .w_full()
            .px(px(6.))
            .gap_2()
            .justify_between()
            .items_center()
            .child(
                h_flex()
                    .id("project-chip")
                    .h(px(22.))
                    .px(px(6.))
                    .gap(px(5.))
                    .items_center()
                    .rounded(px(tokens.radius.row))
                    .cursor_pointer()
                    .when(self.picker == Some(Picker::Project), |this| {
                        this.bg(tokens.colors().row_active())
                    })
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .tooltip(|window, cx| {
                        Tooltip::new(rust_i18n::t!("composer.project.pick").to_string())
                            .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(Picker::Project, cx)))
                    .child(
                        Icon::new(IconName::Folder)
                            .size_3()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(if chosen {
                                tokens.colors().text_secondary
                            } else {
                                tokens.colors().text_muted
                            })
                            .truncate()
                            .child(label),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(11.))
                            .text_color(tokens.colors().text_muted),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .children(indexed.map(|ready| {
                        Button::new("workspace-index")
                            .disabled(self.index_starting)
                            .h(px(22.))
                            .px(px(6.))
                            .rounded(px(tokens.radius.row))
                            .text_size(px(11.))
                            .text_color(index_colour)
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .tooltip(self.index_error.clone().map_or_else(
                                || {
                                    if ready {
                                        rust_i18n::t!("palette.workspace.reindex").to_string()
                                    } else {
                                        rust_i18n::t!("palette.workspace.index").to_string()
                                    }
                                },
                                |error| {
                                    rust_i18n::t!("composer.index.error", error = error).to_string()
                                },
                            ))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.index_workspace(window, cx)),
                            )
                            .child(index_label)
                    }))
                    .child(
                        h_flex()
                            .id("branch-chip")
                            .h(px(22.))
                            .px(px(6.))
                            .gap(px(5.))
                            .items_center()
                            .rounded(px(tokens.radius.row))
                            .when(self.picker == Some(Picker::Branch), |this| {
                                this.bg(tokens.colors().row_active())
                            })
                            .when(branch_picker, |this| {
                                this.cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_branch_picker(window, cx)
                                    }))
                            })
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
                                    .child(branch.unwrap_or_else(|| "—".into())),
                            )
                            .when(branch_picker, |this| {
                                this.child(
                                    Icon::new(IconName::ChevronDown)
                                        .size(px(11.))
                                        .text_color(tokens.colors().text_muted),
                                )
                            }),
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
        let split = self.terminals.split_ids();
        let active_has_output = self
            .terminals
            .active()
            .is_some_and(|tab| !tab.screen.text().is_empty());
        let has_selection = self.quoteable_terminal_selection().is_some();
        let history_offset = self
            .terminals
            .active()
            .map(|tab| tab.screen.display_offset())
            .unwrap_or(0);

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
                            .id("terminal-tabs")
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .overflow_x_scroll()
                            .children(self.terminals.tabs().iter().enumerate().map(
                                |(index, tab)| {
                                    let showing = index == active;
                                    let id = tab.id.clone();
                                    let closing = tab.id.clone();
                                    let confirming = self.terminals.close_confirmation(&tab.id);
                                    let show_label =
                                        rust_i18n::t!("terminal.show", title = tab.title.clone())
                                            .to_string();
                                    let close_label = if confirming {
                                        rust_i18n::t!("terminal.close.confirm").to_string()
                                    } else {
                                        rust_i18n::t!("terminal.close").to_string()
                                    };
                                    h_flex()
                                        .id(SharedString::from(format!("terminal-tab:{}", tab.id)))
                                        .px_2()
                                        .py_1()
                                        .gap_2()
                                        .items_center()
                                        .rounded(px(tokens.radius.row))
                                        .when(showing, |this| this.bg(tokens.colors().row_active()))
                                        .hover(|this| this.bg(tokens.colors().row_hover()))
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "show-terminal:{id}"
                                            )))
                                            .ghost()
                                            .compact()
                                            .accessibility_label(show_label)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.show_terminal(index, window, cx)
                                            }))
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .items_center()
                                                    .child(
                                                        Icon::new(IconName::SquareTerminal)
                                                            .size_3()
                                                            .text_color(
                                                                tokens.colors().text_secondary,
                                                            ),
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
                                                    ),
                                            ),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "close-terminal:{id}"
                                            )))
                                            .ghost()
                                            .compact()
                                            .tooltip(close_label.clone())
                                            .accessibility_label(close_label)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.request_terminal_close(closing.clone(), cx)
                                            }))
                                            .child(
                                                if confirming {
                                                    div()
                                                        .text_xs()
                                                        .text_color(
                                                            tokens.colors().status_attention,
                                                        )
                                                        .child(
                                                            rust_i18n::t!("terminal.close.short")
                                                                .to_string(),
                                                        )
                                                        .into_any_element()
                                                } else {
                                                    Icon::new(IconName::Close)
                                                        .size_3()
                                                        .text_color(tokens.colors().text_muted)
                                                        .into_any_element()
                                                },
                                            ),
                                        )
                                },
                            )),
                    )
                    .children(self.session.is_some().then(|| {
                        let label = rust_i18n::t!("terminal.open").to_string();
                        Button::new("new-terminal")
                            .ghost()
                            .compact()
                            .tooltip(label.clone())
                            .accessibility_label(label)
                            .child(
                                Icon::new(IconName::Plus)
                                    .size_3()
                                    .text_color(tokens.colors().text_muted),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_terminal(window, cx)),
                            )
                    }))
                    .children(split.is_some().then(|| {
                        let label = rust_i18n::t!("terminal.split.focus_other").to_string();
                        Button::new("focus-other-terminal-pane")
                            .ghost()
                            .compact()
                            .tooltip(label.clone())
                            .accessibility_label(label.clone())
                            .label(label)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_other_terminal_pane(window, cx)
                            }))
                    }))
                    .children((!self.terminals.is_empty()).then(|| {
                        let label = if split.is_some() {
                            rust_i18n::t!("terminal.split.close").to_string()
                        } else {
                            rust_i18n::t!("terminal.split.open").to_string()
                        };
                        Button::new("split-terminal")
                            .ghost()
                            .compact()
                            .tooltip(label.clone())
                            .accessibility_label(label)
                            .label(if split.is_some() {
                                rust_i18n::t!("terminal.split.close.short").to_string()
                            } else {
                                rust_i18n::t!("terminal.split.open.short").to_string()
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_terminal_split(window, cx)
                            }))
                    }))
                    .children((!self.terminals.is_empty()).then(|| {
                        Button::new("find-terminal")
                            .ghost()
                            .compact()
                            .tooltip(rust_i18n::t!("terminal.search.open").to_string())
                            .child(Icon::new(IconName::Search).size_3())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_terminal_search(window, cx)
                            }))
                    }))
                    .children(active_has_output.then(|| {
                        let label = rust_i18n::t!("terminal.copy_output").to_string();
                        Button::new("copy-terminal-output")
                            .ghost()
                            .compact()
                            .tooltip(label.clone())
                            .accessibility_label(label)
                            .child(Icon::new(IconName::Copy).size_3())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_terminal_output(cx)))
                    }))
                    .children(has_selection.then(|| {
                        let label = rust_i18n::t!("terminal.quote_selection").to_string();
                        Button::new("quote-terminal-selection")
                            .ghost()
                            .compact()
                            .tooltip(label.clone())
                            .accessibility_label(label.clone())
                            .label(rust_i18n::t!("terminal.quote_selection.short").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.quote_terminal_selection(window, cx)
                            }))
                    }))
                    .children((history_offset > 0).then(|| {
                        Button::new("terminal-live")
                            .ghost()
                            .compact()
                            .tooltip(rust_i18n::t!("terminal.history.live.tooltip").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.terminal_to_live(cx)))
                            .child(
                                rust_i18n::t!(
                                    "terminal.history.lines_back",
                                    count = history_offset
                                )
                                .to_string(),
                            )
                    })),
            )
            .children(self.terminal_search_bar(cx))
            .child(self.terminal_panes(cx))
    }

    /// One terminal viewport, or the two panes of an active split.
    fn terminal_panes(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        if let Some(split) = self.terminals.split_ids() {
            let active = self.terminals.active_id();
            return h_flex()
                .flex_1()
                .min_w_0()
                .children(split.into_iter().enumerate().filter_map(|(index, id)| {
                    let tab = self.terminals.tabs().iter().find(|tab| tab.id == id)?;
                    Some(
                        v_flex()
                            .id(SharedString::from(format!("terminal-pane:{id}")))
                            .flex_1()
                            .min_w_0()
                            .when(active.as_ref() == Some(&id), |this| {
                                this.border_t_1().border_color(tokens.colors().accent)
                            })
                            .when(index > 0, |this| {
                                this.border_l_1()
                                    .border_color(tokens.colors().border_subtle)
                            })
                            .child(self.terminal_screen(&id, &tab.screen, cx)),
                    )
                }))
                .into_any_element();
        }
        match self.terminals.active() {
            Some(tab) => self
                .terminal_screen(&tab.id, &tab.screen, cx)
                .into_any_element(),
            None => self.terminal_start(cx).into_any_element(),
        }
    }

    /// Find controls for the terminal in front.
    fn terminal_search_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let search = self.terminal_search.as_ref()?;
        if self.terminals.active_id().as_ref() != Some(&search.terminal) {
            return None;
        }
        let tokens = Tokens::global(cx).clone();
        let count = search
            .chosen
            .map(|chosen| format!("{} / {}", chosen + 1, search.matches.len()))
            .unwrap_or_else(|| format!("0 / {}", search.matches.len()));
        Some(
            h_flex()
                .w_full()
                .px_2()
                .py_1()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .bg(tokens.colors().bg_surface)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_terminal_search(window, cx);
                    }
                }))
                .child(div().flex_1().child(Input::new(&search.query)))
                .child(
                    div()
                        .min_w(px(58.))
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(count),
                )
                .child(
                    Button::new("previous-terminal-match")
                        .ghost()
                        .disabled(search.matches.is_empty())
                        .tooltip(rust_i18n::t!("transcript.search.previous").to_string())
                        .child(Icon::new(IconName::ArrowUp).size_3())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.step_terminal_search(ginka_ui::search::Direction::Previous, cx)
                        })),
                )
                .child(
                    Button::new("next-terminal-match")
                        .ghost()
                        .disabled(search.matches.is_empty())
                        .tooltip(rust_i18n::t!("transcript.search.next").to_string())
                        .child(Icon::new(IconName::ArrowDown).size_3())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.step_terminal_search(ginka_ui::search::Direction::Next, cx)
                        })),
                )
                .child(
                    Button::new("close-terminal-search")
                        .ghost()
                        .tooltip(rust_i18n::t!("transcript.search.close").to_string())
                        .child(Icon::new(IconName::Close).size_3())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_terminal_search(window, cx)
                        })),
                )
                .into_any_element(),
        )
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
        terminal: &ginka_protocol::TerminalId,
        screen: &ginka_ui::terminal::TerminalScreen,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = cx.theme_mono_font();
        let selected = self.terminal_search.as_ref().and_then(|search| {
            (search.terminal == *terminal)
                .then(|| search.chosen.and_then(|chosen| search.matches.get(chosen)))
                .flatten()
        });
        let rows = screen.rows_of_cells_with_match(selected);
        let selection = self
            .terminal_selection
            .as_ref()
            .filter(|drag| drag.terminal == *terminal)
            .map(|drag| drag.selection);
        let worktree = self.session.as_ref().map(|row| row.path.clone());
        let active = self.terminals.active_id().as_ref() == Some(terminal);
        let focus_terminal = terminal.clone();
        let drag_terminal = terminal.clone();
        let bounds_terminal = terminal.clone();
        let shell = cx.entity();
        let scroll_terminal = terminal.clone();
        let terminal_key = terminal.clone();

        v_flex()
            .id(SharedString::from(format!("terminal-screen:{terminal}")))
            .flex_1()
            .px_2()
            .py_1()
            .overflow_hidden()
            .font_family(mono)
            .text_size(px(12.5))
            .line_height(px(17.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.begin_terminal_selection(&focus_terminal, event.position, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                this.extend_terminal_selection(&drag_terminal, event, cx)
            }))
            .on_prepaint(move |bounds, _, cx| {
                shell.update(cx, |this, _| {
                    this.terminal_bounds.insert(bounds_terminal.clone(), bounds);
                });
            })
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                this.scroll_terminal(&scroll_terminal, event, cx)
            }))
            .when(active, |this| {
                this.track_focus(&self.terminal_focus)
                    .key_context("Terminal")
                    .on_action(cx.listener(Self::paste_into_terminal))
                    .on_action(cx.listener(Self::on_copy_terminal_output))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        this.type_into_terminal(event, cx)
                    }))
            })
            .children(rows.into_iter().enumerate().map(|(row_index, row)| {
                let text = row.iter().map(|cell| cell.text).collect::<String>();
                let links = worktree
                    .as_deref()
                    .map(|root| ginka_ui::terminal::file_links(&text, root))
                    .unwrap_or_default();
                let mut elements = Vec::new();
                let mut column = 0;
                while column < row.len() {
                    if let Some(link) = links.iter().find(|link| link.columns.start == column) {
                        let end = link.columns.end.min(row.len());
                        let path = link.path.clone();
                        let click_link = link.clone();
                        let location = match (link.line, link.column) {
                            (Some(line), Some(column)) => {
                                format!("{}:{line}:{column}", link.path)
                            }
                            (Some(line), None) => format!("{}:{line}", link.path),
                            _ => link.path.clone(),
                        };
                        let label =
                            rust_i18n::t!("terminal.open_file", location = location).to_string();
                        elements.push(
                            Button::new(SharedString::from(format!(
                                "terminal-file:{terminal_key}:{row_index}:{column}"
                            )))
                            .text()
                            .h(px(17.))
                            .p_0()
                            .accessibility_label(label)
                            .child(h_flex().children(
                                row[column..end].iter().cloned().enumerate().map(
                                    |(offset, cell)| {
                                        terminal_cell(
                                            cell,
                                            &tokens,
                                            true,
                                            selection.is_some_and(|selection| {
                                                selection.contains(
                                                    ginka_ui::terminal::TerminalPoint::new(
                                                        row_index,
                                                        column + offset,
                                                    ),
                                                )
                                            }),
                                        )
                                    },
                                ),
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(range) = click_link.selection_range()
                                    && let Some(workspace) =
                                        this.session.as_ref().map(|row| row.workspace.clone())
                                {
                                    this.open_definition(
                                        &workspace,
                                        ginka_ui::editor::DefinitionTarget {
                                            path: path.clone(),
                                            range,
                                        },
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.open_file(path.clone(), false, window, cx);
                                }
                            }))
                            .into_any_element(),
                        );
                        column = end;
                    } else {
                        elements.push(terminal_cell(
                            row[column].clone(),
                            &tokens,
                            false,
                            selection.is_some_and(|selection| {
                                selection.contains(ginka_ui::terminal::TerminalPoint::new(
                                    row_index, column,
                                ))
                            }),
                        ));
                        column += 1;
                    }
                }
                h_flex().children(elements)
            }))
    }

    fn center(&self, selected_text: Option<String>, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .child(self.transcript(selected_text, cx))
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

/// Draw one terminal cell without giving the view ownership of terminal state.
fn terminal_cell(
    cell: ginka_ui::terminal::ScreenCell,
    tokens: &Tokens,
    linked: bool,
    selected: bool,
) -> AnyElement {
    div()
        .when(cell.search_match, |this| {
            this.bg(tokens.colors().accent.opacity(0.35))
        })
        .when(cell.cursor, |this| {
            this.bg(tokens.colors().text_primary)
                .text_color(tokens.colors().bg_terminal)
        })
        .when(!cell.cursor, |this| {
            this.text_color(
                cell.foreground
                    .map(|colour| terminal_colour(colour, tokens))
                    .unwrap_or(tokens.colors().text_primary),
            )
            .when_some(cell.background, |this, colour| {
                this.bg(terminal_colour(colour, tokens))
            })
            .when(linked, |this| {
                this.text_color(tokens.colors().accent).underline()
            })
        })
        .when(cell.bold, |this| this.font_semibold())
        .when(cell.italic, |this| this.italic())
        .when(cell.underline, |this| this.underline())
        .when(selected && !cell.cursor, |this| {
            this.bg(tokens.colors().accent.opacity(0.45))
        })
        .child(if cell.text == ' ' {
            // A space with no width is a hole in a painted bar.
            SharedString::from("\u{00a0}")
        } else {
            SharedString::from(cell.text.to_string())
        })
        .into_any_element()
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
    let (showing, wants_changes, wants_usage, wants_history) = this
        .update(cx, |this, cx| {
            let open = this.surfaces.read(cx).open_surface();
            (
                this.session.as_ref().map(|row| row.workspace.clone()),
                // Only while the surface that shows them is open: reading a
                // diff runs git over the whole worktree, and a panel nobody
                // opened is not worth that on every tick.
                open == Some(ginka_ui::surface::Surface::Git),
                open == Some(ginka_ui::surface::Surface::Reports),
                open == Some(ginka_ui::surface::Surface::Git)
                    && this.surfaces.read(cx).history_is_open(),
            )
        })
        .map_err(|_| ())?;
    let (
        rows,
        projects,
        agents,
        accounts,
        plans,
        usage,
        checkpoints,
        changes,
        staged_changes,
        comments,
        history,
    ) = cx
        .background_spawn(async move {
            let rows = listing.workspaces(crate::daemon::now()).await;
            // Separately from the workspaces: a project with no worktree is
            // still a heading a chat can be started under.
            let projects = listing.projects().await;
            // Cached by the daemon, so this is a request rather than two
            // subprocesses per agent every tick.
            let agents = listing.agents().await;
            // The logins and their gauges: cached and pushed by the daemon
            // respectively, so both are a request rather than a probe.
            let accounts = listing.accounts().await;
            let plans = listing.plans().await;
            let usage = if wants_usage {
                listing.usage(30).await
            } else {
                None
            };
            let checkpoints = match &showing {
                Some(workspace) => listing.checkpoints(workspace).await,
                None => Vec::new(),
            };
            let (changes, staged_changes, comments) = match (&showing, wants_changes) {
                (Some(workspace), true) => (
                    listing
                        .changes(workspace, ginka_protocol::ChangeSource::Unstaged)
                        .await,
                    listing
                        .changes(workspace, ginka_protocol::ChangeSource::Staged)
                        .await,
                    listing.comments(workspace).await,
                ),
                _ => (None, None, Vec::new()),
            };
            let history = match (&showing, wants_history) {
                (Some(workspace), true) => listing.history(workspace, 50).await,
                _ => Vec::new(),
            };
            (
                rows,
                projects,
                agents,
                accounts,
                plans,
                usage,
                checkpoints,
                changes,
                staged_changes,
                comments,
                history,
            )
        })
        .await;
    tracing::debug!(
        rows = rows.len(),
        agents = agents.len(),
        "refreshed workspaces"
    );
    this.update(cx, |this, cx| {
        this.agents = agents;
        this.accounts = accounts;
        this.plans = plans;
        this.checkpoints = checkpoints;
        if let Some(usage) = usage {
            this.surfaces
                .update(cx, |surfaces, cx| surfaces.set_usage(usage, cx));
        }
        this.sync_footer(cx);
        this.projects = projects.iter().map(ProjectRow::from_project).collect();
        let listed = this.projects.clone();
        this.sidebar
            .update(cx, |sidebar, cx| sidebar.set_projects(listed, cx));
        if wants_changes {
            this.surfaces.update(cx, |surfaces, cx| {
                let staged = staged_changes
                    .as_ref()
                    .map(|changes| changes.files.iter().map(|file| file.path.clone()).collect())
                    .unwrap_or_default();
                surfaces.set_changes(changes, cx);
                surfaces.set_staged_changes(staged_changes, cx);
                surfaces.set_staged(staged, cx);
                surfaces.set_comments(comments, cx);
                surfaces.set_history(history, cx);
            });
        }
        this.sidebar
            .update(cx, |sidebar, cx| sidebar.set_rows(rows, cx));
        match this.sidebar.read(cx).selected_row().cloned() {
            Some(row) => {
                this.target_project = Some(ProjectName(row.origin.to_string()));
                this.session = Some(row);
            }
            // Nothing selected is the home screen, which is where a window
            // opens and where "new chat" leaves it.
            None if this.sidebar.read(cx).selection().is_none() => this.session = None,
            // Selected but not listed yet: a workspace this window has just
            // made, which the next refresh will name.
            None => {}
        }
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

/// Re-read one session's ordered follow-up queue after a push or navigation.
async fn pull_queue(
    this: &WeakEntity<Shell>,
    link: &Arc<DaemonLink>,
    session: SessionId,
    cx: &mut AsyncApp,
) -> Result<(), ()> {
    let listing = link.clone();
    let requested = session.clone();
    let (messages, can_send_now) = cx
        .background_spawn(async move { listing.queued_messages(&requested).await })
        .await;
    this.update(cx, |this, cx| {
        if this.session.as_ref().and_then(|row| row.session.as_ref()) == Some(&session) {
            this.queued_messages = messages;
            this.queue_can_send_now = can_send_now;
            this.queue_error = None;
            cx.notify();
        }
    })
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
        if let Some(workspace) = self.session.as_ref().map(|row| row.workspace.clone()) {
            self.surfaces.update(cx, |surfaces, cx| {
                surfaces.set_workspace(workspace, window, cx)
            });
        }
        // Copied out: the headers below bind listeners through `cx`, and a
        // borrow of the theme held across that is a borrow held across the
        // whole window.
        let tokens = Tokens::global(cx).clone();
        let sidebar_open = self.layout.is_open(Panel::Sidebar);
        let right_open = self.layout.is_open(Panel::RightPanel);
        let project_selected = self.sidebar.read(cx).has_project_selection();
        let sidebar_width = navigator_width(project_selected, self.layout.size(Panel::Sidebar));
        let sidebar_range = if project_selected {
            px(420.)..px(720.)
        } else {
            PROJECT_RAIL_WIDTH..PROJECT_RAIL_WIDTH
        };
        let right_width = self.layout.size(Panel::RightPanel);
        // Built before the column chain: both headers bind listeners, and the
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
        let selected_text = TextSelection::selected_text(window, cx);
        let selected_text = ginka_ui::transcript::selected_quote(&selected_text).map(str::to_owned);
        let centre = self.center(selected_text, cx).into_any_element();

        v_flex()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_right_panel))
            .on_action(cx.listener(Self::on_toggle_terminal_dock))
            .on_action(cx.listener(Self::on_toggle_palette))
            .on_action(cx.listener(Self::on_find_transcript))
            .on_action(cx.listener(Self::on_next_surface))
            .on_action(cx.listener(Self::on_previous_surface))
            .on_action(cx.listener(Self::on_next_terminal_tab))
            .on_action(cx.listener(Self::on_previous_terminal_tab))
            .on_action(cx.listener(Self::on_navigate_back))
            .on_action(cx.listener(Self::on_navigate_forward))
            .on_action(cx.listener(Self::on_switch_session))
            .size_full()
            // No background here: `Root` already paints the translucent window
            // and painting it again composites the alpha away.
            .text_color(tokens.colors().text_primary)
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
                                    .size_range(sidebar_range)
                                    .child(
                                        // The column runs to the top of the
                                        // window and carries the window's own
                                        // controls; the strip paints the
                                        // sidebar's background and border so
                                        // the two read as one surface, and
                                        // neither paints over the other.
                                        v_flex()
                                            .size_full()
                                            .children(window_controls)
                                            .child(self.sidebar.clone())
                                            .into_any_element(),
                                    ),
                            )
                        })
                        .child(
                            resizable_panel().child(
                                v_flex()
                                    .size_full()
                                    .child(column_header)
                                    .child(centre)
                                    .into_any_element(),
                            ),
                        )
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
            .children(self.image_markup_view(cx))
            .children(self.add_project_view(cx))
            .children(self.add_account_view(cx))
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
                                            let selected_text = this
                                                .palette
                                                .as_ref()
                                                .and_then(|palette| palette.selected_text.clone());
                                            this.palette = None;
                                            this.run_command(
                                                command.clone(),
                                                selected_text.as_deref(),
                                                window,
                                                cx,
                                            );
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

/// The add-login dialog's own state while it is open.
struct AddAccount {
    /// The driver id the login is for.
    provider: String,
    id: Entity<InputState>,
    label: Entity<InputState>,
    /// What the daemon said when it refused, shown until the next attempt.
    error: Option<String>,
    /// Set while the daemon is being asked, so Return twice is one request.
    busy: bool,
}

/// The add-project modal's state while it is open.
struct AddProject {
    /// Reader-facing name stored separately from the stable path-derived key.
    name: Entity<InputState>,
    /// Source folder selected through the platform picker.
    path: Option<PathBuf>,
    /// What validation or the daemon refused.
    error: Option<String>,
    /// Set while the daemon is being asked, so Return twice is one request.
    busy: bool,
    /// Keeps the name field driving the modal's validation state.
    _name_changed: Subscription,
}

/// The palette's own state while it is open.
struct Palette {
    query: Entity<InputState>,
    /// What has been typed, kept here so the filter is not a read of the input
    /// on every frame.
    typed: String,
    /// Which entry Return would run.
    chosen: usize,
    /// Text selected before the palette took focus, retained for quote actions.
    selected_text: Option<String>,
}

/// What the access chip and its rows call a mode.
fn access_label(mode: ginka_protocol::AccessMode) -> String {
    match mode {
        ginka_protocol::AccessMode::ReadOnly => rust_i18n::t!("composer.access.read_only"),
        ginka_protocol::AccessMode::Ask => rust_i18n::t!("composer.access.ask"),
        ginka_protocol::AccessMode::Auto => rust_i18n::t!("composer.access.auto"),
    }
    .to_string()
}

/// What a mode lets the agent do, in a row's note.
fn access_note(mode: ginka_protocol::AccessMode) -> String {
    match mode {
        ginka_protocol::AccessMode::ReadOnly => rust_i18n::t!("composer.access.read_only.note"),
        ginka_protocol::AccessMode::Ask => rust_i18n::t!("composer.access.ask.note"),
        ginka_protocol::AccessMode::Auto => rust_i18n::t!("composer.access.auto.note"),
    }
    .to_string()
}
