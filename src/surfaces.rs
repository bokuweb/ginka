//! The right panel: `docs/ui.md` §3.4.
//!
//! A surface is whatever the user wants beside the transcript — git, files,
//! the browser, reports, skills. Each open surface is a tab of a `DockArea`,
//! so tabs can be dragged into another order, into a group of their own, or
//! beside one another; the arrangement is kept per workspace.

use ginka_protocol::model::{
    ChangeKind, Changes, Checkpoint, ContentMatch, FileContent, FileEntry, GitCommit, LineKind,
    Skill, SkillScope, WorkspaceContentMatch, WorkspaceFileMatch,
};
use ginka_protocol::{ProjectName, WorkspaceId};
use ginka_ui::Tokens;
use ginka_ui::commit_selection::{CommitMessageGeneration, CommitSelection};
use ginka_ui::diff_filter::DiffFilter;
use ginka_ui::dock::{DockNode, SurfaceDock};
use ginka_ui::editor::{
    DefinitionTarget, FileTabs, PreviewKind, SaveState, definition_selection, language_for_path,
    markdown_preview, preview_kind, save_state, saved_selection_reference, selected_text,
};
use ginka_ui::file_explorer::{ExplorerOpen, FileExplorer};
use ginka_ui::file_search::FileSearchScope;
use ginka_ui::image_preview::ImagePreview;
use ginka_ui::skills::{ScopeFilter, SkillFilter, StateFilter};
use ginka_ui::surface::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::Selectable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::dock::{
    BasePanel, DockArea, DockEvent, DockLayout, Panel, PanelControl, PanelEvent, panel_handle,
};
use gpui_component::input::{
    Editor, EditorState, InputEvent, InputState, Replace, Search, TabSize, Textarea, TextareaState,
};
use gpui_component::text::TextView;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

/// How long the arrangement has to stay still before it is saved: the dock
/// reports every step of a drag.
const ARRANGEMENT_SETTLES: Duration = Duration::from_millis(400);

/// A comment being written in the Git surface: its file, the line it starts
/// on, where a shift-clicked range ends, and its text box.
type OpenComment = (String, Option<u32>, Option<u32>, Entity<TextareaState>);

struct FileBuffer {
    workspace: WorkspaceId,
    file: FileContent,
    editor: Option<Entity<EditorState>>,
    image_view: Option<Entity<ImagePreview>>,
    complaint: Option<SharedString>,
    previewing: bool,
    /// When the reader last typed in it, for autosave.
    last_edit: Option<std::time::Instant>,
    /// A save is on its way to the daemon.
    saving: bool,
    /// The daemon refused the last save — the file changed on disk — so it
    /// is not saved again without the reader.
    conflicted: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FileOpenMode {
    Visit,
    History,
}

pub struct SurfacePanel {
    /// The surface most recently brought forward, which the palette's
    /// next/previous steps from.
    open: Option<Surface>,
    /// How the open surfaces are arranged, at rest.
    dock: SurfaceDock,
    /// The dock area the arrangement is drawn and dragged in.
    dock_area: Entity<DockArea>,
    /// One panel per surface, kept so a rebuilt dock reuses them.
    dock_tabs: HashMap<Surface, Entity<SurfaceTab>>,
    /// The dock area is to be rebuilt from `dock` on the next render, which
    /// is the first place a window is to hand.
    dock_stale: bool,
    /// The chooser is up in place of the dock.
    choosing: bool,
    /// The panel fills the window in place of the other columns.
    maximized: bool,
    /// Counts arrangement changes, so only the last of a burst is saved.
    arrangement_changes: u64,
    /// The shell's terminals, drawn in the Terminal tab; the shell owns them.
    terminal_view: Option<AnyView>,
    /// Native browser view on platforms supported by the toolkit.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    browser_tabs: HashMap<WorkspaceId, Result<Entity<crate::browser::BrowserPane>, SharedString>>,
    /// Workspace whose native browser is currently visible.
    browser_workspace: Option<WorkspaceId>,
    /// Native child views sit above GPUI overlays and must yield to a modal.
    browser_suspended: bool,
    /// Latest inspected element, held above the native child view until sent.
    browser_capture: Option<ginka_core::browser::BrowserCapture>,
    /// Requested design change paired with the inspected element.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    browser_feedback: Entity<TextareaState>,
    /// What the workspace on screen has changed, as the shell last read it.
    changes: Option<Changes>,
    /// What is already in the index, shown separately from worktree-only edits.
    staged_changes: Option<Changes>,
    /// View-only path filter shared by current changes and the commit reader.
    diff_filter: DiffFilter,
    /// Explicit whole-file commit scope, independent of staging and filtering.
    commit_selection: CommitSelection,
    /// Surrounding lines requested for each displayed diff hunk.
    diff_context: u8,
    /// Input for the view-only diff path filter.
    diff_finder: Entity<InputState>,
    /// Recent commits for the selected workspace, already bounded by the daemon.
    history: Vec<GitCommit>,
    /// Whether the reader asked to see recent commits above the current diff.
    history_open: bool,
    /// Completed turns for the selected workspace, newest first.
    checkpoints: Vec<Checkpoint>,
    /// Whether the turn list is visible above the current diff.
    turns_open: bool,
    /// A completed turn and its exact saved-start to saved-end diff.
    turn_view: Option<(Checkpoint, Option<Result<Changes, String>>)>,
    /// A commit opened from the history, and what it did once that is read.
    /// Shown in place of the uncommitted diff while it is set.
    commit_view: Option<(GitCommit, Option<Changes>)>,
    /// Where the last pull request is, or why it could not be opened.
    pull_request: Option<(SharedString, bool)>,
    /// A pull request is being opened.
    opening_pull_request: bool,
    /// The last plain push was refused, so overwriting the remote with a
    /// lease is offered — and `true` in the second field once the reader has
    /// asked once and must confirm.
    lease_push: Option<bool>,
    /// The complaint on screen is a refused commit, which the agent can be
    /// asked to fix.
    commit_refused: bool,
    /// The workspace's branch has an open pull request, so its checks can
    /// be handed to the agent.
    open_pull_request: bool,
    /// The pull request's checks are listed under the remote actions.
    checks_open: bool,
    /// Its checks, failures first, or why they could not be read; `None`
    /// while they are being read.
    checks: Option<Result<Vec<ginka_protocol::model::CheckRun>, SharedString>>,
    /// The file being opened was picked with a single click, so it goes in
    /// the preview tab.
    preview_next: bool,
    /// Changed images read for a side-by-side look, by change source and
    /// path: the image before and after (Orca's image diff).
    image_diffs: HashMap<(String, String), (Option<String>, Option<String>)>,
    /// The file whose diff is expanded. A review starts as a list of files:
    /// twelve diffs at once is not a review, it is a wall.
    expanded: Option<String>,
    /// Diffs side by side — the old file on the left — rather than unified.
    split: bool,
    /// The comments waiting to go back to the agent.
    comments: Vec<ginka_protocol::model::ReviewComment>,
    /// The line a comment is being written on, and the box it is written in.
    ///
    /// One at a time: a review is read line by line, and two open boxes is a
    /// form, not a margin note.
    ///
    /// The second line is where a shift-click range ends, when it is one.
    commenting: Option<OpenComment>,
    /// The paths that are staged for the next commit.
    ///
    /// Read separately from the changes themselves: the uncommitted diff is
    /// what the reader is reading, and whether a file is in the next commit is
    /// a second question about the same file.
    staged: Vec<String>,
    /// The file whose revert has been offered and is waiting to be confirmed.
    ///
    /// Two steps, like a rewind: a revert deletes a file the agent wrote, and
    /// git has nothing to undo it with.
    reverting: Option<String>,
    /// The exact unstaged hunk whose destructive discard awaits confirmation.
    reverting_hunk: Option<String>,
    /// The commit message being written, if the box is open.
    message: Option<Entity<TextareaState>>,
    /// Why the last commit did not happen.
    complaint: Option<SharedString>,
    /// An agent is writing the message; the button says so meanwhile.
    generation: CommitMessageGeneration,
    /// A message that arrived while the box was drawn: put into it on the
    /// next render, which is the first place a window is to hand.
    generated: Option<String>,
    /// What is typed into the file finder.
    finder: Entity<InputState>,
    /// The paths that match it, best first.
    files: Vec<FileEntry>,
    /// Virtualized explorer owns bounded catalogue and keyboard navigation.
    file_explorer: Entity<FileExplorer>,
    /// Workspace whose asynchronous tree response may replace the catalogue.
    tree_workspace: Option<WorkspaceId>,
    /// The lines that contain what was typed, if any do.
    matches: Vec<ContentMatch>,
    /// Whether the finder reads this worktree or every active one in its project.
    file_search_scope: FileSearchScope,
    /// Project-wide hits, tagged with the worktree that owns each path.
    project_matches: Vec<WorkspaceContentMatch>,
    /// Project-wide fuzzy path hits, tagged with their owning worktree.
    project_files: Vec<WorkspaceFileMatch>,
    /// Identity of the latest project request, rejecting older async answers.
    project_search: Option<(ProjectName, String)>,
    /// Open file paths and the tab currently in front.
    file_tabs: FileTabs,
    /// The independently editable buffer behind each open tab.
    file_buffers: Vec<FileBuffer>,
    /// Whether the finder is in front while open tabs remain behind it.
    browsing_files: bool,
    /// Workspace and path whose asynchronous read may replace the panel next.
    opening: Option<(WorkspaceId, String, FileOpenMode)>,
    /// Cross-file definition to select after its buffer has been opened.
    definition: Option<(WorkspaceId, DefinitionTarget)>,
    /// Whether daemon-host worktree paths may launch a process on this machine.
    local_paths: bool,
    /// What the work cost and where each login stands, as the shell last
    /// read it. `None` until the Reports surface has been opened.
    usage: Option<ginka_ui::reports::UsageReport>,
    /// The selected project's skills plus the user's own, as last read from
    /// the daemon. `None` while the first read is in flight.
    skills: Option<Vec<Skill>>,
    /// What is typed into the skills finder.
    skill_finder: Entity<InputState>,
    /// Fields for creating one shared skill.
    skill_name: Entity<InputState>,
    skill_description: Entity<InputState>,
    skill_body: Entity<TextareaState>,
    /// Whether the new skill belongs to the selected project.
    skill_create_project: bool,
    skill_project_available: bool,
    skill_creating: bool,
    /// Scope and enablement facets composed with the skills finder.
    skill_filter: SkillFilter,
    /// The daemon stopped its bounded skill scan before visiting every root.
    skills_truncated: bool,
    /// The grouped skill whose every installed copy is being changed.
    skill_changing: Option<String>,
    /// Why the most recent skill read or mutation failed.
    skill_error: Option<SharedString>,
}

/// Emitted when the panel wants the shell to do something only it can.
/// What a commit is followed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitThen {
    Nothing,
    Push,
    PullRequest,
}

pub enum SurfaceEvent {
    /// Re-read the current diff with a different number of surrounding lines.
    DiffContextChanged {
        commit: Option<GitCommit>,
        turn: Option<Checkpoint>,
    },
    /// The surfaces open in the right panel, or their arrangement, changed
    /// and should be remembered.
    Arranged,
    /// Let the panel fill the window, or give the other columns back.
    ToggleMaximized,
    /// Close the panel, as its toggle in the window's header does.
    Close,
    /// A page loaded in a workspace's browser: keep it in its history.
    /// Only a platform with the browser surface sends this or the next.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    BrowserVisited {
        workspace: WorkspaceId,
        url: String,
        title: Option<String>,
    },
    /// A workspace's address bar changed: find pages to offer.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    BrowserTyped {
        workspace: WorkspaceId,
        query: String,
    },
    /// Commit the workspace's work with this message.
    ///
    /// `only_staged` when the reader has staged something: having said which
    /// files belong in the commit, they do not expect the rest to come along.
    Commit {
        message: String,
        only_staged: bool,
        /// What happens once the commit is in: nothing, a push, or a push
        /// and a pull request — MonoCode's commit menu.
        then: CommitThen,
        /// Fold into the last commit rather than adding one; an empty
        /// `message` then keeps that commit's.
        amend: bool,
        /// Literal whole-file selection; empty retains the normal staged/all scope.
        paths: Vec<String>,
    },
    /// Have an agent write the message (§3.3 N9). It arrives later, as an
    /// event the shell hands back through [`SurfacePanel::set_generated`].
    GenerateCommitMessage {
        /// Fresh id identifying the result allowed to update this draft.
        generation_id: String,
        only_staged: bool,
        paths: Vec<String>,
    },
    /// Fast-forward the workspace from its configured upstream.
    Pull,
    /// Pull then push in one action, or publish a branch never pushed.
    Sync,
    /// Push the workspace branch, creating its upstream when needed.
    Push,
    /// Replace the remote branch with rewritten history, if the remote is
    /// still what was last fetched. Offered only after a plain push failed.
    PushWithLease,
    /// Hand a refused commit — what git and its hooks said, and the message
    /// it was going to use — to the workspace's agent to fix.
    FixCommit { message: String, output: String },
    /// Hand the failing checks of the branch's pull request to the agent.
    FixChecks,
    /// Read the checks of the branch's pull request; they come back
    /// through [`Surfaces::set_checks`].
    LoadChecks,
    /// Read a changed image's two sides under the change source it is
    /// shown in: staged, unstaged, a commit or a turn.
    LoadImageDiff {
        path: String,
        old_path: Option<String>,
        source: ginka_protocol::model::ChangeSource,
    },
    /// Refresh recent commits after the reader expands history.
    RefreshHistory,
    /// Read what one commit did; it comes back through
    /// [`SurfacePanel::show_commit`].
    OpenCommit(GitCommit),
    /// Read exactly one completed turn's saved snapshots.
    OpenTurn(Checkpoint),
    /// Push the branch and open a pull request for it.
    CreatePullRequest,
    /// Push and open a pull request whose title and description an agent
    /// wrote from the branch's commits and diff.
    CreateGeneratedPullRequest,
    /// Leave a comment on a file, and a line of it.
    Comment {
        path: String,
        line: Option<u32>,
        /// The last line of a shift-clicked range.
        end_line: Option<u32>,
        text: String,
    },
    /// Send every waiting comment back to the agent.
    SendReview,
    /// Take a waiting review comment back before it is sent.
    RemoveComment { id: String },
    /// Look for files whose path matches this.
    FindFiles(String),
    /// Read a file and show it.
    OpenFile(String),
    /// Select another workspace and open one of its project-search hits.
    OpenWorkspaceFile {
        workspace: WorkspaceId,
        path: String,
    },
    /// Open and select a definition returned by the active language server.
    OpenDefinition {
        workspace: WorkspaceId,
        target: DefinitionTarget,
    },
    /// Reload a closed file selected by back/forward history.
    OpenFileFromHistory(String),
    /// Save an editor buffer against the revision it was opened from.
    SaveFile {
        workspace: WorkspaceId,
        path: String,
        text: String,
        expected_revision: String,
    },
    /// Open a saved file at the active editor line on the daemon host.
    OpenExternalEditor {
        workspace: WorkspaceId,
        path: String,
        line: Option<u32>,
    },
    /// Add an editor selection's exact source location to the chat draft.
    AddFileReference(String),
    /// Paste an editor selection into the active terminal.
    WriteTerminalSelection(String),
    /// Add one sanitized inspect-mode bundle to the active chat draft.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    AddBrowserContext(String),
    /// Read the selected project's skills plus the user's own.
    RefreshSkills,
    /// Create a shared skill in the user or selected project scope.
    CreateSkill {
        /// Lowercase slug for the new skill directory.
        name: String,
        /// One-line front matter summary.
        description: String,
        /// Markdown instructions for the agent.
        body: String,
        /// Whether the selected project owns the skill.
        project: bool,
    },
    /// Set every installed copy of a grouped skill to one state.
    SetSkillEnabled { name: String, enabled: bool },
    /// Put a file into the next commit, or take it back out.
    Stage { path: String, staged: bool },
    /// Put one exact hunk into the next commit, or take it back out.
    StageHunk {
        path: String,
        header: String,
        staged: bool,
    },
    /// Permanently discard one exact unstaged hunk.
    RevertHunk { path: String, header: String },
    /// Throw away a file's uncommitted work.
    Revert { path: String },
}

impl EventEmitter<SurfaceEvent> for SurfacePanel {}

impl SurfacePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, local_paths: bool) -> Self {
        let finder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.files.search").to_string())
        });
        // Searched as it is typed: a finder that waited for a return key would
        // be a form, and the list is what tells the user whether the next
        // character is worth typing.
        cx.subscribe(&finder, |_, finder, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = finder.read(cx).value().to_string();
                cx.emit(SurfaceEvent::FindFiles(query));
            }
        })
        .detach();
        let file_explorer = cx.new(FileExplorer::new);
        cx.subscribe(&file_explorer, |this, _, event: &ExplorerOpen, cx| {
            if !event.preview {
                this.file_tabs.pin(&event.path);
            }
            this.preview_next = event.preview;
            cx.emit(SurfaceEvent::OpenFile(event.path.clone()));
        })
        .detach();
        let diff_finder = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("surface.git.filter").to_string())
        });
        cx.subscribe(&diff_finder, |this, finder, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.diff_filter.query = finder.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let skill_finder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.skills.search").to_string())
        });
        let skill_name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.skills.create.name").to_string())
        });
        let skill_description = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.skills.create.description").to_string())
        });
        let skill_body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.skills.create.body").to_string())
        });
        cx.subscribe(&skill_finder, |this, finder, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.skill_filter.query = finder.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let browser_feedback = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.browser.feedback").to_string())
        });
        let dock_area = crate::surface_dock::dock_area("surfaces", Some(1), window, cx);
        cx.subscribe(&dock_area, |this, area, event: &DockEvent, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.read_dock(&area, cx);
            }
        })
        .detach();
        Self {
            dock: SurfaceDock::empty(),
            dock_area,
            dock_tabs: HashMap::new(),
            dock_stale: false,
            choosing: false,
            maximized: false,
            arrangement_changes: 0,
            terminal_view: None,
            finder,
            files: Vec::new(),
            file_explorer,
            tree_workspace: None,
            matches: Vec::new(),
            file_search_scope: FileSearchScope::default(),
            project_matches: Vec::new(),
            project_files: Vec::new(),
            project_search: None,
            file_tabs: FileTabs::default(),
            file_buffers: Vec::new(),
            browsing_files: true,
            opening: None,
            definition: None,
            local_paths,
            open: None,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            browser_tabs: HashMap::new(),
            browser_workspace: None,
            browser_suspended: false,
            browser_capture: None,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            browser_feedback,
            changes: None,
            staged_changes: None,
            diff_filter: DiffFilter::default(),
            commit_selection: CommitSelection::default(),
            diff_context: 3,
            diff_finder,
            history: Vec::new(),
            history_open: false,
            checkpoints: Vec::new(),
            turns_open: false,
            turn_view: None,
            commit_view: None,
            pull_request: None,
            opening_pull_request: false,
            lease_push: None,
            commit_refused: false,
            open_pull_request: false,
            checks_open: false,
            checks: None,
            preview_next: false,
            image_diffs: HashMap::new(),
            expanded: None,
            split: false,
            comments: Vec::new(),
            commenting: None,
            staged: Vec::new(),
            reverting: None,
            reverting_hunk: None,
            message: None,
            generation: CommitMessageGeneration::default(),
            generated: None,
            complaint: None,
            usage: None,
            skills: None,
            skill_finder,
            skill_name,
            skill_description,
            skill_body,
            skill_create_project: false,
            skill_project_available: false,
            skill_creating: false,
            skill_filter: SkillFilter::default(),
            skills_truncated: false,
            skill_changing: None,
            skill_error: None,
        }
    }

    /// Hand the panel a completed daemon read of the skill library.
    pub fn set_skills(
        &mut self,
        result: Result<(Vec<Skill>, bool), String>,
        cx: &mut Context<Self>,
    ) {
        self.skill_changing = None;
        self.skill_creating = false;
        match result {
            Ok((skills, truncated)) => {
                self.skills = Some(skills);
                self.skills_truncated = truncated;
                self.skill_error = None;
            }
            Err(error) => self.skill_error = Some(error.into()),
        }
        cx.notify();
    }

    /// Mark one grouped skill busy until the daemon has changed every copy.
    pub fn begin_skill_change(&mut self, name: String, cx: &mut Context<Self>) {
        self.skill_changing = Some(name);
        self.skill_error = None;
        cx.notify();
    }

    /// Mark a new skill busy while the daemon writes and re-reads it.
    pub fn begin_skill_create(&mut self, cx: &mut Context<Self>) {
        self.skill_creating = true;
        self.skill_error = None;
        cx.notify();
    }

    /// Keep the project scope available only while a project is selected.
    pub fn set_skill_project_available(&mut self, available: bool, cx: &mut Context<Self>) {
        self.skill_project_available = available;
        if !available {
            self.skill_create_project = false;
        }
        cx.notify();
    }

    /// Clear a previous project's library while a new daemon read begins.
    pub fn begin_skill_refresh(&mut self, cx: &mut Context<Self>) {
        self.skills = None;
        self.skill_creating = false;
        self.skill_changing = None;
        self.skill_error = None;
        cx.notify();
    }

    /// Hand the panel the usage report. Called from the shell's refresh while
    /// the Reports surface is open.
    pub fn set_usage(&mut self, usage: ginka_ui::reports::UsageReport, cx: &mut Context<Self>) {
        if self.usage.as_ref() != Some(&usage) {
            self.usage = Some(usage);
            cx.notify();
        }
    }

    /// A login's windows were read again: the report shows the new reading
    /// without waiting for the next refresh.
    pub fn set_plan(
        &mut self,
        snapshot: ginka_protocol::model::PlanSnapshot,
        cx: &mut Context<Self>,
    ) {
        if let Some(usage) = self.usage.as_mut() {
            usage.set_plan(snapshot);
            cx.notify();
        }
    }

    /// Hand the panel the comments waiting in this workspace.
    pub fn set_comments(
        &mut self,
        comments: Vec<ginka_protocol::model::ReviewComment>,
        cx: &mut Context<Self>,
    ) {
        if self.comments != comments {
            self.comments = comments;
            cx.notify();
        }
    }

    /// Hand the panel the files that matched what was typed.
    pub fn set_files(&mut self, files: Vec<FileEntry>, cx: &mut Context<Self>) {
        if self.files != files {
            self.files = files;
            cx.notify();
        }
    }

    /// Hand the panel the bounded catalogue behind its hierarchy.
    pub fn set_file_tree(
        &mut self,
        workspace: WorkspaceId,
        files: Vec<FileEntry>,
        truncated: bool,
        cx: &mut Context<Self>,
    ) {
        if self.tree_workspace.as_ref() != Some(&workspace) {
            return;
        }
        self.file_explorer
            .update(cx, |explorer, cx| explorer.set_files(files, truncated, cx));
    }

    /// Clear search-only rows as soon as the empty query restores the tree.
    pub fn begin_file_tree(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        self.tree_workspace = Some(workspace);
        self.files.clear();
        self.matches.clear();
        cx.notify();
    }

    /// Hand the panel the lines that matched what was typed.
    pub fn set_matches(&mut self, matches: Vec<ContentMatch>, cx: &mut Context<Self>) {
        if self.matches != matches {
            self.matches = matches;
            cx.notify();
        }
    }

    /// Current content-search scope selected by the reader.
    pub fn file_search_scope(&self) -> FileSearchScope {
        self.file_search_scope
    }

    /// Mark a project query current before its asynchronous answer arrives.
    pub fn begin_project_search(
        &mut self,
        project: ProjectName,
        query: String,
        cx: &mut Context<Self>,
    ) {
        self.project_search = Some((project, query));
        self.files.clear();
        self.matches.clear();
        self.project_matches.clear();
        self.project_files.clear();
        cx.notify();
    }

    /// Accept project hits only while they still describe the visible query.
    pub fn set_project_matches(
        &mut self,
        project: ProjectName,
        query: String,
        files: Vec<WorkspaceFileMatch>,
        matches: Vec<WorkspaceContentMatch>,
        cx: &mut Context<Self>,
    ) {
        if self.project_search.as_ref() != Some(&(project, query)) {
            return;
        }
        self.project_files = files;
        self.project_matches = matches;
        cx.notify();
    }

    fn set_file_search_scope(&mut self, scope: FileSearchScope, cx: &mut Context<Self>) {
        if self.file_search_scope == scope {
            return;
        }
        self.file_search_scope = scope;
        self.files.clear();
        self.matches.clear();
        self.project_matches.clear();
        self.project_files.clear();
        self.project_search = None;
        let query = self.finder.read(cx).value().to_string();
        cx.emit(SurfaceEvent::FindFiles(query));
        cx.notify();
    }

    /// Open a file picked from the tree or the search results: a single
    /// click previews it in the reusable tab, a double click keeps it
    /// (MonoCode's preview tabs). The keyboard keeps it too.
    fn open_from_list(&mut self, path: String, event: &ClickEvent, cx: &mut Context<Self>) {
        let single = matches!(event, ClickEvent::Mouse(click) if click.up.click_count < 2);
        if !single {
            self.file_tabs.pin(&path);
        }
        self.preview_next = single;
        cx.emit(SurfaceEvent::OpenFile(path));
    }

    /// Mark the workspace and path whose asynchronous read is current.
    ///
    /// Returns whether the daemon must be asked. `from_history` restores the
    /// target without turning that reload into a new visit.
    pub fn begin_file_open(
        &mut self,
        workspace: WorkspaceId,
        path: String,
        from_history: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.definition = None;
        if self
            .file_buffers
            .iter()
            .any(|buffer| buffer.workspace == workspace && buffer.file.path == path)
        {
            if from_history {
                self.file_tabs.restore(path);
            } else if std::mem::take(&mut self.preview_next) {
                self.file_tabs.open_preview(path);
            } else {
                self.file_tabs.open(path);
            }
            self.browsing_files = false;
            cx.notify();
            return false;
        }
        let mode = if from_history {
            FileOpenMode::History
        } else {
            FileOpenMode::Visit
        };
        self.opening = Some((workspace, path, mode));
        cx.notify();
        true
    }

    /// Mark a cross-file definition as the next file visit.
    ///
    /// Returns whether the daemon must read the target. An already-open buffer
    /// is focused and selected synchronously.
    pub fn begin_definition_open(
        &mut self,
        workspace: WorkspaceId,
        target: DefinitionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let should_read = self.begin_file_open(workspace.clone(), target.path.clone(), false, cx);
        self.definition = Some((workspace.clone(), target.clone()));
        if !should_read {
            self.apply_definition(&workspace, &target.path, window, cx);
        }
        should_read
    }

    /// Forget a file and any in-flight read when its workspace leaves screen.
    pub fn clear_file(&mut self, cx: &mut Context<Self>) {
        self.file_tabs.clear();
        self.file_buffers.clear();
        self.files.clear();
        self.matches.clear();
        self.project_matches.clear();
        self.project_files.clear();
        self.project_search = None;
        self.file_explorer
            .update(cx, |explorer, cx| explorer.clear(cx));
        self.tree_workspace = None;
        self.browsing_files = true;
        self.opening = None;
        self.definition = None;
        cx.notify();
    }

    /// Show a current file read and make complete text files editable.
    pub fn set_file(
        &mut self,
        workspace: WorkspaceId,
        worktree: std::path::PathBuf,
        path: String,
        file: Option<FileContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((opening_workspace, opening_path, mode)) = self.opening.as_ref() else {
            return;
        };
        if opening_workspace != &workspace || opening_path != &path {
            return;
        }
        let mode = *mode;
        self.opening = None;
        let Some(file) = file else {
            // A failed read opens nothing, so the preview it was for is not
            // left waiting to catch the next, unrelated open.
            self.preview_next = false;
            self.definition = None;
            cx.notify();
            return;
        };
        let surface = cx.entity().downgrade();
        let definition_workspace = workspace.clone();
        let local_paths = self.local_paths;
        let editor = (!file.binary && !file.truncated).then(|| {
            let editor = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(language_for_path(&file.path))
                    .folding(true)
                    .tab_size(TabSize {
                        tab_size: 4,
                        ..Default::default()
                    })
                    .default_value(file.text.clone())
            });
            let lsp = crate::lsp::EditorLspBinding::default();
            let lsp_for_changes = lsp.clone();
            let edited_path = file.path.clone();
            cx.subscribe(&editor, move |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.note_edit(&edited_path, cx);
                    let text = editor.read(cx).value().to_string();
                    let (version, server) = lsp_for_changes.change_target();
                    if let Some(server) = server {
                        cx.background_spawn(async move {
                            if let Err(error) =
                                smol::unblock(move || server.change(version, text)).await
                            {
                                tracing::debug!(%error, "language-server change was not delivered");
                            }
                        })
                        .detach();
                    }
                    cx.notify();
                }
            })
            .detach();
            cx.observe(&editor, |_, _, cx| cx.notify()).detach();
            let show_definition = Rc::new(move |target, cx: &mut App| {
                let _ = surface.update(cx, |_, cx| {
                    cx.emit(SurfaceEvent::OpenDefinition {
                        workspace: definition_workspace.clone(),
                        target,
                    });
                });
            });
            if local_paths {
                crate::lsp::attach(
                    editor.clone(),
                    lsp,
                    crate::lsp::EditorLspDocument::new(
                        worktree,
                        file.path.clone(),
                        file.text.clone(),
                        show_definition,
                    ),
                    window,
                    cx,
                );
            }
            editor
        });
        let image_view = ImagePreview::from_file(&file, cx).map(|preview| cx.new(|_| preview));
        self.file_buffers.push(FileBuffer {
            workspace: workspace.clone(),
            file,
            editor,
            image_view,
            complaint: None,
            previewing: false,
            last_edit: None,
            saving: false,
            conflicted: false,
        });
        let path = self
            .file_buffers
            .last()
            .expect("a file buffer was just inserted")
            .file
            .path
            .clone();
        match mode {
            FileOpenMode::Visit if std::mem::take(&mut self.preview_next) => {
                // The previewed file this one replaces: its buffer goes
                // with its tab, unless it holds unsaved work, in which case
                // it was kept the moment it was edited and is not displaced.
                if let Some(displaced) = self.file_tabs.open_preview(path.clone()) {
                    self.file_buffers
                        .retain(|buffer| buffer.file.path != displaced);
                }
            }
            FileOpenMode::Visit => self.file_tabs.open(path.clone()),
            FileOpenMode::History => self.file_tabs.restore(path.clone()),
        }
        self.apply_definition(&workspace, &path, window, cx);
        self.browsing_files = false;
        cx.notify();
    }

    fn apply_definition(
        &mut self,
        workspace: &WorkspaceId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pending_workspace, target)) = self.definition.as_ref() else {
            return;
        };
        if pending_workspace != workspace || target.path != path {
            return;
        }
        let range = target.range;
        self.definition = None;
        let Some(editor) = self
            .file_buffers
            .iter()
            .find(|buffer| &buffer.workspace == workspace && buffer.file.path == path)
            .and_then(|buffer| buffer.editor.clone())
        else {
            return;
        };
        editor.update(cx, |editor, cx| {
            let selection = definition_selection(&editor.value(), range);
            editor.set_selected_range(selection, cx);
            editor.focus(window, cx);
        });
    }

    /// Accept the daemon's new revision after a successful save.
    pub fn set_file_saved(
        &mut self,
        workspace: &WorkspaceId,
        file: FileContent,
        cx: &mut Context<Self>,
    ) {
        if let Some(buffer) = self
            .file_buffers
            .iter_mut()
            .find(|buffer| &buffer.workspace == workspace && buffer.file.path == file.path)
        {
            buffer.file = file;
            buffer.complaint = None;
            buffer.saving = false;
            buffer.conflicted = false;
            cx.notify();
        }
    }

    /// The reader typed in `path`: autosave it once typing pauses for
    /// [`ginka_ui::editor::AUTOSAVE_AFTER`] (MonoCode's autosave).
    fn note_edit(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(buffer) = self
            .file_buffers
            .iter_mut()
            .find(|buffer| buffer.file.path == path)
        else {
            return;
        };
        buffer.last_edit = Some(std::time::Instant::now());
        // An edited preview is kept: it is no longer only being looked at.
        self.file_tabs.pin(path);
        let path = path.to_string();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(ginka_ui::editor::AUTOSAVE_AFTER)
                .await;
            this.update(cx, |this, cx| this.autosave(&path, cx)).ok();
        })
        .detach();
    }

    /// Save `path` if [`ginka_ui::editor::autosave_due`] says so — the timer
    /// of an earlier keystroke finds the buffer still being typed in, and
    /// does nothing.
    fn autosave(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(buffer) = self
            .file_buffers
            .iter_mut()
            .find(|buffer| buffer.file.path == path)
        else {
            return;
        };
        let Some(editor) = buffer.editor.as_ref() else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        let state = save_state(&buffer.file, &text);
        if !ginka_ui::editor::autosave_due(
            state,
            buffer.last_edit,
            std::time::Instant::now(),
            buffer.conflicted,
            buffer.saving,
        ) {
            return;
        }
        buffer.saving = true;
        cx.emit(SurfaceEvent::SaveFile {
            workspace: buffer.workspace.clone(),
            path: buffer.file.path.clone(),
            text,
            expected_revision: buffer.file.revision.clone(),
        });
    }

    /// Keep the editor intact and explain why its save was refused.
    pub fn set_file_save_error(
        &mut self,
        workspace: &WorkspaceId,
        path: &str,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(buffer) = self
            .file_buffers
            .iter_mut()
            .find(|buffer| &buffer.workspace == workspace && buffer.file.path == path)
        {
            buffer.complaint = Some(error.into());
            buffer.saving = false;
            // Not saved over by itself again: a manual save is the reader
            // deciding to.
            buffer.conflicted = true;
            cx.notify();
        }
    }

    fn active_file(&self) -> Option<&FileBuffer> {
        let path = self.file_tabs.active()?;
        self.file_buffers
            .iter()
            .find(|buffer| buffer.file.path == path)
    }

    fn focus_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.file_tabs.focus(path);
        self.browsing_files = false;
        cx.notify();
    }

    fn browse_files(&mut self, cx: &mut Context<Self>) {
        self.browsing_files = true;
        cx.notify();
    }

    fn toggle_file_preview(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(buffer) = self
            .file_buffers
            .iter_mut()
            .find(|buffer| buffer.file.path == path && preview_kind(&buffer.file).is_some())
        {
            buffer.previewing = !buffer.previewing;
            cx.notify();
        }
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.file_tabs.go_back() {
            self.browsing_files = false;
            if !self
                .file_buffers
                .iter()
                .any(|buffer| buffer.file.path == path)
            {
                cx.emit(SurfaceEvent::OpenFileFromHistory(path));
            }
            cx.notify();
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.file_tabs.go_forward() {
            self.browsing_files = false;
            if !self
                .file_buffers
                .iter()
                .any(|buffer| buffer.file.path == path)
            {
                cx.emit(SurfaceEvent::OpenFileFromHistory(path));
            }
            cx.notify();
        }
    }

    fn close_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(buffer) = self
            .file_buffers
            .iter()
            .find(|buffer| buffer.file.path == path)
        else {
            return;
        };
        let state = buffer
            .editor
            .as_ref()
            .map(|editor| save_state(&buffer.file, &editor.read(cx).value()))
            .unwrap_or(SaveState::ReadOnly);
        if !self.file_tabs.close(path, state) {
            self.file_tabs.focus(path);
            self.browsing_files = false;
            if let Some(buffer) = self
                .file_buffers
                .iter_mut()
                .find(|buffer| buffer.file.path == path)
            {
                buffer.complaint = Some(
                    rust_i18n::t!("surface.files.close_dirty")
                        .to_string()
                        .into(),
                );
            }
            cx.notify();
            return;
        }
        self.file_buffers.retain(|buffer| buffer.file.path != path);
        self.browsing_files = self.file_tabs.active().is_none();
        cx.notify();
    }

    /// Hand the panel the paths that are staged for the next commit.
    pub fn set_staged(&mut self, staged: Vec<String>, cx: &mut Context<Self>) {
        if self.staged != staged {
            self.staged = staged;
            cx.notify();
        }
    }

    /// Whether the commit box would commit only part of what is on screen.
    fn only_staged(&self) -> bool {
        !self.commit_selection.active() && !self.staged.is_empty()
    }

    /// Resolve selection against both sections, regardless of the path filter.
    fn commit_files(&self) -> impl Iterator<Item = &ginka_protocol::model::FileChange> {
        self.changes
            .iter()
            .chain(self.staged_changes.iter())
            .flat_map(|changes| changes.files.iter())
    }

    /// An empty explicit selection must never fall back to committing everything.
    fn valid_commit_scope(&mut self, cx: &mut Context<Self>) -> bool {
        if self.commit_selection.active()
            && self.commit_selection.paths(self.commit_files()).is_empty()
        {
            self.complaint = Some(
                rust_i18n::t!("surface.git.needs_selection")
                    .to_string()
                    .into(),
            );
            cx.notify();
            return false;
        }
        true
    }

    /// What the agent wrote, or why it could not (`CommitMessageGenerated`).
    pub fn set_generated(
        &mut self,
        generation_id: Option<&str>,
        message: Option<String>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.generation.finish(generation_id) {
            return;
        }
        match message {
            Some(message) => self.generated = Some(message.trim_end().to_string()),
            None => {
                self.complaint = error.map(SharedString::from);
            }
        }
        cx.notify();
    }

    /// Say why a commit did not happen, or clear it once one did.
    pub fn set_commit_result(&mut self, complaint: Option<String>, cx: &mut Context<Self>) {
        self.complaint = complaint.map(SharedString::from);
        self.commit_refused = self.complaint.is_some();
        if self.complaint.is_none() {
            // It went in; the message belongs to the commit now.
            self.message = None;
            self.commit_selection.clear();
        }
        cx.notify();
    }

    /// Show a remote-sync error without disturbing a commit message draft.
    pub fn set_git_sync_result(&mut self, complaint: Option<String>, cx: &mut Context<Self>) {
        self.complaint = complaint.map(SharedString::from);
        self.commit_refused = false;
        cx.notify();
    }

    /// A changed image's two sides arrived, as `data:` URLs.
    pub fn set_image_diff(
        &mut self,
        source: &ginka_protocol::model::ChangeSource,
        path: String,
        before: Option<String>,
        after: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.image_diffs
            .insert((image_source_key(source), path), (before, after));
        cx.notify();
    }

    /// A binary file's expanded row: its images side by side once read,
    /// and until then the word "binary" and a way to read them (Orca's image
    /// diff). `source` is the change the row is shown under.
    fn image_diff_view(
        &self,
        file: &ginka_protocol::model::FileChange,
        source: &ginka_protocol::model::ChangeSource,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let key = (image_source_key(source), file.path.clone());
        match self.image_diffs.get(&key).cloned() {
            Some((before, after)) => h_flex()
                .w_full()
                .px_3()
                .py_2()
                .gap_3()
                .children(
                    [
                        (
                            rust_i18n::t!("surface.git.image_before").to_string(),
                            before,
                        ),
                        (rust_i18n::t!("surface.git.image_after").to_string(), after),
                    ]
                    .into_iter()
                    .map(|(label, url)| {
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .child(label),
                            )
                            .child(match url {
                                Some(url) => div()
                                    .h(px(180.))
                                    .rounded(px(tokens.radius.row))
                                    .bg(tokens.colors().bg_surface)
                                    .child(
                                        img(SharedString::from(url))
                                            .size_full()
                                            .object_fit(ObjectFit::Contain),
                                    )
                                    .into_any_element(),
                                None => div()
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .child(rust_i18n::t!("surface.git.image_missing").to_string())
                                    .into_any_element(),
                            })
                    }),
                )
                .into_any_element(),
            None => {
                let (path, old_path, source) =
                    (file.path.clone(), file.old_path.clone(), source.clone());
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("surface.git.binary").to_string()),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "show-images:{}:{}",
                            key.0, file.path
                        )))
                        .ghost()
                        .compact()
                        .small()
                        .label(rust_i18n::t!("surface.git.show_images").to_string())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(SurfaceEvent::LoadImageDiff {
                                path: path.clone(),
                                old_path: old_path.clone(),
                                source: source.clone(),
                            })
                        })),
                    )
                    .into_any_element()
            }
        }
    }

    /// Whether the workspace's branch has an open pull request.
    pub fn set_open_pull_request(&mut self, open: bool) {
        self.open_pull_request = open;
    }

    /// The checks read for `workspace`'s pull request, or why they could
    /// not be. Dropped when the reader has moved to another workspace since.
    pub fn set_checks(
        &mut self,
        workspace: &WorkspaceId,
        checks: Result<Vec<ginka_protocol::model::CheckRun>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.browser_workspace.as_ref() != Some(workspace) {
            return;
        }
        self.checks = Some(
            checks
                .map(|checks| ginka_ui::checks::ordered(&checks))
                .map_err(SharedString::from),
        );
        cx.notify();
    }

    /// Say how a push went: a refused plain push offers the lease push,
    /// anything that went through takes the offer away.
    pub fn set_push_result(
        &mut self,
        complaint: Option<String>,
        with_lease: bool,
        cx: &mut Context<Self>,
    ) {
        self.lease_push = match (&complaint, with_lease) {
            (Some(_), false) => Some(false),
            _ => None,
        };
        self.set_git_sync_result(complaint, cx);
    }

    /// Show what a commit did. `changes` is `None` while it is being read; a
    /// late answer for a commit the reader has since left is dropped.
    pub fn show_commit(
        &mut self,
        commit: GitCommit,
        changes: Option<Changes>,
        cx: &mut Context<Self>,
    ) {
        if changes.is_some()
            && self
                .commit_view
                .as_ref()
                .is_none_or(|(shown, _)| shown.id != commit.id)
        {
            return;
        }
        self.expanded = changes
            .as_ref()
            .and_then(|changes| changes.files.first())
            .map(|file| file.path.clone());
        self.commit_view = Some((commit, changes));
        self.turn_view = None;
        cx.notify();
    }

    /// Number of unchanged lines currently requested for Git diffs.
    pub fn diff_context(&self) -> u8 {
        self.diff_context
    }

    /// Show a completed turn, dropping a response if the reader has moved on.
    pub fn show_turn(
        &mut self,
        checkpoint: Checkpoint,
        changes: Option<Result<Changes, String>>,
        cx: &mut Context<Self>,
    ) {
        if changes.is_some()
            && self
                .turn_view
                .as_ref()
                .is_none_or(|(shown, _)| shown.id != checkpoint.id)
        {
            return;
        }
        self.expanded = changes
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|changes| changes.files.first())
            .map(|file| file.path.clone());
        self.turn_view = Some((checkpoint, changes));
        self.commit_view = None;
        cx.notify();
    }

    /// The commit a pull request was waiting on did not happen; the button
    /// stops saying it is opening one. The commit's own refusal says why.
    pub fn abandon_pull_request(&mut self, cx: &mut Context<Self>) {
        self.opening_pull_request = false;
        cx.notify();
    }

    /// Where the pull request is, or why it could not be opened.
    pub fn set_pull_request(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        self.opening_pull_request = false;
        self.pull_request = Some(match result {
            Ok(url) => (url.into(), true),
            Err(error) => (error.into(), false),
        });
        cx.notify();
    }

    /// Bring a surface forward, opening a tab for it when it has none: what
    /// the chooser and the palette do when they are asked for one.
    pub fn show(&mut self, surface: Surface, cx: &mut Context<Self>) {
        let before = self.dock.visible();
        let rearranged = self.dock.open(surface);
        let changed = rearranged || self.open != Some(surface) || self.choosing;
        self.open = Some(surface);
        self.choosing = false;
        if rearranged {
            self.dock_stale = true;
        }
        if !before.contains(&surface) {
            self.arrived(surface, cx);
        }
        if changed {
            cx.emit(SurfaceEvent::Arranged);
        }
        cx.notify();
    }

    /// What a surface needs the first time it comes on screen.
    fn arrived(&mut self, surface: Surface, cx: &mut Context<Self>) {
        // A finder that opens empty is one the user has to type into before it
        // says anything; the start of the list is what a picker shows before
        // anything is typed.
        if surface == Surface::Files && self.files.is_empty() {
            cx.emit(SurfaceEvent::FindFiles(String::new()));
        }
        if surface == Surface::Skills {
            self.skills = None;
            self.skill_error = None;
            cx.emit(SurfaceEvent::RefreshSkills);
        }
    }

    /// Put the chooser up in place of the dock, or take it down again.
    fn toggle_chooser(&mut self, cx: &mut Context<Self>) {
        self.choosing = !self.choosing;
        cx.notify();
    }

    /// Restore one workspace's arrangement without treating it as a change.
    pub fn restore_dock(&mut self, dock: SurfaceDock, cx: &mut Context<Self>) {
        if self.dock != dock {
            self.dock = dock;
            self.dock_stale = true;
        }
        let visible = self.dock.visible();
        if !self.open.is_some_and(|open| visible.contains(&open)) {
            self.open = visible.first().copied();
        }
        self.choosing = false;
        cx.notify();
    }

    /// Hand the panel the view that draws the shell's terminals.
    pub fn set_terminal_view(&mut self, view: AnyView) {
        self.terminal_view = Some(view);
    }

    /// The arrangement as it is saved for the workspace on screen.
    pub fn arrangement(&self) -> Option<ginka_core::settings::SurfaceArrangement> {
        self.dock.save()
    }

    /// Whether a surface is on screen: the front tab of one of the groups.
    pub fn shows(&self, surface: Surface) -> bool {
        self.dock.visible().contains(&surface)
    }

    /// Whether the native browser view may be drawn: it sits above every GPUI
    /// element, so it must be hidden whenever anything covers its place.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn browser_on_screen(&self) -> bool {
        self.shows(Surface::Browser) && !self.browser_suspended && !self.choosing
    }

    /// Adopt what the dock area holds after the user dragged, closed or
    /// switched a tab.
    fn read_dock(&mut self, area: &Entity<DockArea>, cx: &mut Context<Self>) {
        // A rebuild is on its way; what the area holds now is about to go.
        if self.dock_stale {
            return;
        }
        let dock = SurfaceDock::from_panel_state(&area.read(cx).dump(cx).center);
        if dock == self.dock {
            return;
        }
        let before = self.dock.visible();
        self.dock = dock;
        let visible = self.dock.visible();
        for surface in &visible {
            if !before.contains(surface) {
                self.open = Some(*surface);
                self.arrived(*surface, cx);
            }
        }
        if !self.open.is_some_and(|open| visible.contains(&open)) {
            self.open = visible.first().copied();
        }
        self.arrangement_changes += 1;
        let change = self.arrangement_changes;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ARRANGEMENT_SETTLES).await;
            this.update(cx, |this, cx| {
                if this.arrangement_changes == change {
                    cx.emit(SurfaceEvent::Arranged);
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Rebuild the dock area from the arrangement at rest.
    fn rebuild_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.dock.root().cloned() else {
            return;
        };
        let panel = cx.entity();
        let mut surfaces = Vec::new();
        collect_surfaces(&root, &mut surfaces);
        for surface in surfaces {
            self.dock_tabs
                .entry(surface)
                .or_insert_with(|| cx.new(|cx| SurfaceTab::new(surface, &panel, cx)));
        }
        let layout = self.dock_layout(&root, cx);
        // The centre is always a split; a lone group sits inside one.
        let layout = match root {
            DockNode::Tabs { .. } => DockLayout::h_split().child(layout, None),
            DockNode::Split { .. } => layout,
        };
        self.dock_area
            .update(cx, |area, cx| area.set_center(layout, window, cx));
    }

    fn dock_layout(&self, node: &DockNode, cx: &App) -> DockLayout {
        match node {
            DockNode::Split {
                vertical,
                children,
                sizes,
            } => {
                let split = if *vertical {
                    DockLayout::v_split()
                } else {
                    DockLayout::h_split()
                };
                children
                    .iter()
                    .enumerate()
                    .fold(split, |split, (index, child)| {
                        split.child(
                            self.dock_layout(child, cx),
                            sizes.get(index).copied().flatten().map(px),
                        )
                    })
            }
            DockNode::Tabs { surfaces, active } => surfaces
                .iter()
                .filter_map(|surface| self.dock_tabs.get(surface))
                .fold(DockLayout::tabs(), |tabs, tab| {
                    tabs.panel_view(panel_handle(tab.clone()), cx)
                })
                .active_index(*active),
        }
    }

    /// One surface's content, drawn into its tab.
    fn surface_body(&mut self, surface: Surface, cx: &mut Context<Self>) -> AnyElement {
        match surface {
            Surface::Git => self.git(cx).into_any_element(),
            Surface::Files => self.files(cx).into_any_element(),
            Surface::Browser => self.browser(cx),
            Surface::Reports => self.reports(cx).into_any_element(),
            Surface::Skills => self.skills(cx).into_any_element(),
            Surface::Terminal => match &self.terminal_view {
                Some(view) => view.clone().into_any_element(),
                None => self.placeholder(surface, cx).into_any_element(),
            },
        }
    }

    /// Select the browser tab owned by a workspace, creating it lazily.
    pub fn set_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.browser_workspace.as_ref() == Some(&workspace) {
            return;
        }
        // Another workspace, another pull request.
        self.generation.clear();
        self.generated = None;
        self.commit_selection.clear();
        self.checks_open = false;
        self.checks = None;
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = window;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if let Some(previous) = self
                .browser_workspace
                .as_ref()
                .and_then(|workspace| self.browser_tabs.get(workspace))
                .and_then(|browser| browser.as_ref().ok())
            {
                previous.update(cx, |browser, cx| browser.set_visible(false, cx));
            }
            if !self.browser_tabs.contains_key(&workspace) {
                let created =
                    crate::browser::BrowserPane::create(window, cx).map_err(SharedString::from);
                if let Ok(browser) = &created {
                    let expected = workspace.clone();
                    cx.subscribe(browser, move |this, _, event, cx| {
                        if this.browser_workspace.as_ref() != Some(&expected) {
                            return;
                        }
                        match event {
                            crate::browser::BrowserEvent::Inspected(capture) => {
                                this.browser_capture = Some((**capture).clone());
                                cx.notify();
                            }
                            crate::browser::BrowserEvent::Visited { url, title } => {
                                cx.emit(SurfaceEvent::BrowserVisited {
                                    workspace: expected.clone(),
                                    url: url.clone(),
                                    title: title.clone(),
                                });
                            }
                            crate::browser::BrowserEvent::Typed(query) => {
                                cx.emit(SurfaceEvent::BrowserTyped {
                                    workspace: expected.clone(),
                                    query: query.clone(),
                                });
                            }
                        }
                    })
                    .detach();
                }
                self.browser_tabs.insert(workspace.clone(), created);
            }
            if let Some(browser) = self
                .browser_tabs
                .get(&workspace)
                .and_then(|browser| browser.as_ref().ok())
            {
                let visible = self.browser_on_screen();
                browser.update(cx, |browser, cx| browser.set_visible(visible, cx));
            }
        }
        self.browser_workspace = Some(workspace);
        self.browser_capture = None;
        cx.notify();
    }

    /// Offer these pages under a workspace's address bar.
    pub fn set_browser_suggestions(
        &mut self,
        workspace: &WorkspaceId,
        pages: Vec<ginka_protocol::model::VisitedPage>,
        cx: &mut Context<Self>,
    ) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(Ok(browser)) = self.browser_tabs.get(workspace) {
            browser.update(cx, |browser, cx| browser.set_suggestions(pages, cx));
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = (workspace, pages, cx);
    }

    /// Which surface is showing, so the shell knows what to keep fetching.
    pub fn open_surface(&self) -> Option<Surface> {
        self.open
    }

    /// Hide a native browser while GPUI application chrome must cover it.
    pub fn suspend_browser(&mut self, suspended: bool, cx: &mut Context<Self>) {
        if self.browser_suspended != suspended {
            self.browser_suspended = suspended;
            cx.notify();
        }
    }

    /// Whether periodic refresh should include recent commit history.
    pub fn history_is_open(&self) -> bool {
        self.history_open
    }

    /// Hand the panel what changed. Called from the shell's refresh.
    pub fn set_changes(&mut self, changes: Option<Changes>, cx: &mut Context<Self>) {
        if self.changes != changes {
            self.changes = changes;
            // A side read before this change may no longer be the image.
            self.image_diffs.clear();
            cx.notify();
        }
    }

    /// Hand the panel the index-only diff, separate from worktree edits.
    pub fn set_staged_changes(&mut self, changes: Option<Changes>, cx: &mut Context<Self>) {
        if self.staged_changes != changes {
            self.staged_changes = changes;
            // A side read before this change may no longer be the image.
            self.image_diffs.clear();
            cx.notify();
        }
    }

    /// Hand the Git surface the selected workspace's recent commits.
    pub fn set_history(&mut self, history: Vec<GitCommit>, cx: &mut Context<Self>) {
        if self.history != history {
            self.history = history;
            cx.notify();
        }
    }

    /// Update the turn picker from daemon-owned checkpoints.
    pub fn set_checkpoints(&mut self, checkpoints: Vec<Checkpoint>, cx: &mut Context<Self>) {
        if self.checkpoints != checkpoints {
            if self.turn_view.as_ref().is_some_and(|(shown, _)| {
                !checkpoints
                    .iter()
                    .any(|checkpoint| checkpoint.id == shown.id)
            }) {
                self.turn_view = None;
            }
            self.checkpoints = checkpoints;
            cx.notify();
        }
    }

    /// The strip across the top of the panel.
    ///
    /// The window has no title bar (`docs/ui.md` §3.1), so this is the right
    /// column's own header and it is the same height as the other two — three
    /// strips at three heights would read as three windows.
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_shrink_0()
            .h(ginka_ui::layout::HEADER_HEIGHT)
            .px_3()
            // Filling the window, the strip starts where the traffic lights
            // are, and keeps clear of them the way the centre's does.
            .when(self.maximized, |this| {
                this.pl(ginka_ui::layout::TRAFFIC_LIGHT_INSET)
            })
            .justify_between()
            .items_center()
            .child(
                Button::new("choose-surface")
                    .icon(IconName::Plus)
                    .ghost()
                    .compact()
                    .small()
                    .tooltip(rust_i18n::t!("surface.choose").to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_chooser(cx))),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("maximize-surfaces")
                            .icon(if self.maximized {
                                IconName::Minimize
                            } else {
                                IconName::Maximize
                            })
                            .ghost()
                            .compact()
                            .small()
                            .tooltip(if self.maximized {
                                rust_i18n::t!("surface.restore").to_string()
                            } else {
                                rust_i18n::t!("surface.maximize").to_string()
                            })
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::ToggleMaximized)),
                            ),
                    )
                    .child(
                        Button::new("close-surfaces")
                            .icon(IconName::PanelRight)
                            .ghost()
                            .compact()
                            .small()
                            .tooltip(rust_i18n::t!("surface.close_panel").to_string())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::Close))),
                    ),
            )
    }

    /// Say whether the panel fills the window, so its control says which way
    /// it goes and its header keeps clear of the window's own controls.
    pub fn set_maximized(&mut self, maximized: bool, cx: &mut Context<Self>) {
        if self.maximized != maximized {
            self.maximized = maximized;
            cx.notify();
        }
    }

    fn chooser_button(&self, surface: Surface, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .id(surface.label())
            .w_full()
            .px_3p5()
            .py_3()
            .gap_2p5()
            .items_center()
            .rounded(px(tokens.radius.panel))
            .bg(tokens.colors().bg_surface)
            .border_1()
            .border_color(tokens.colors().border_subtle)
            .hover(|this| this.bg(tokens.colors().bg_raised))
            .child(
                surface
                    .icon()
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.colors().text_primary)
                    .child(surface.title()),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.show(surface, cx)))
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_5()
            .px_8()
            .child(
                v_flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .text_lg()
                            .text_color(tokens.colors().text_primary)
                            .child(rust_i18n::t!("surface.empty.title").to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("surface.empty.hint").to_string()),
                    ),
            )
            .child(
                v_flex().w_full().max_w(px(360.)).gap_2().children(
                    Surface::ALL
                        .iter()
                        .map(|surface| self.chooser_button(*surface, cx).into_any_element()),
                ),
            )
    }

    /// What the agent changed, file by file.
    fn git(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        if let Some(view) = self.read_only_diff_view(cx) {
            return view;
        }
        let tokens = Tokens::global(cx).clone();
        let Some(changes) = self.changes.clone() else {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.reading").to_string()),
                )
                .into_any_element();
        };
        let Some(staged_changes) = self.staged_changes.clone() else {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.reading").to_string()),
                )
                .into_any_element();
        };

        if changes.is_empty() && staged_changes.is_empty() {
            return v_flex()
                .flex_1()
                .child(self.git_remote_actions(cx))
                .children(self.pull_request_line(cx))
                .children(self.git_checks(cx))
                .when(self.history_open, |this| this.child(self.git_history(cx)))
                .when(self.turns_open, |this| this.child(self.git_turns(cx)))
                .child(self.diff_filter_input(cx))
                .child(self.diff_context_control(cx))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.clean").to_string()),
                )
                .into_any_element();
        }

        let all_files = changes.files.iter().chain(&staged_changes.files);
        let files = all_files
            .clone()
            .map(|file| file.path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let visible = self.diff_filter.visible_count(all_files.clone());
        let (added, removed) = all_files
            .filter(|file| self.diff_filter.matches(file))
            .fold((0, 0), |(added, removed), file| {
                (added + file.added, removed + file.removed)
            });
        v_flex()
            .id("git-surface")
            .flex_1()
            .overflow_y_scroll()
            .child(self.git_remote_actions(cx))
            .children(self.pull_request_line(cx))
            .children(self.git_checks(cx))
            .when(self.history_open, |this| this.child(self.git_history(cx)))
            .when(self.turns_open, |this| this.child(self.git_turns(cx)))
            .child(self.diff_filter_input(cx))
            .child(self.diff_context_control(cx))
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(if self.diff_filter.query.trim().is_empty() {
                                rust_i18n::t!("surface.git.summary", files = files).to_string()
                            } else {
                                rust_i18n::t!(
                                    "surface.git.filtered_summary",
                                    visible = visible,
                                    total = files
                                )
                                .to_string()
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_done)
                            .child(format!("+{added}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(format!("-{removed}")),
                    ),
            )
            .when(visible == 0, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_6()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.no_matches").to_string()),
                )
            })
            .when(
                changes
                    .files
                    .iter()
                    .any(|file| self.diff_filter.matches(file)),
                |this| this.child(self.change_section(&changes, false, cx)),
            )
            .when(
                staged_changes
                    .files
                    .iter()
                    .any(|file| self.diff_filter.matches(file)),
                |this| this.child(self.change_section(&staged_changes, true, cx)),
            )
            .children(self.review_bar(cx))
            .child(self.commit_box(cx))
            .into_any_element()
    }

    /// Filter the diff rows by path without changing daemon-owned review state.
    fn diff_filter_input(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        div()
            .w_full()
            .p_2()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(ginka_ui::field::input(&self.diff_finder))
    }

    /// Switch the number of context lines without changing Git's review state.
    fn diff_context_control(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        h_flex()
            .w_full()
            .px_3()
            .py_1()
            .gap_1()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.git.context").to_string()),
            )
            .children([3_u8, 10, 25].map(|lines| {
                Button::new(format!("diff-context-{lines}"))
                    .ghost()
                    .compact()
                    .small()
                    .label(if self.diff_context == lines {
                        format!("{lines} ✓")
                    } else {
                        lines.to_string()
                    })
                    .tooltip(rust_i18n::t!("surface.git.context_tooltip").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.diff_context == lines {
                            return;
                        }
                        this.diff_context = lines;
                        this.reverting_hunk = None;
                        cx.emit(SurfaceEvent::DiffContextChanged {
                            commit: this.commit_view.as_ref().map(|(commit, _)| commit.clone()),
                            turn: this.turn_view.as_ref().map(|(turn, _)| turn.clone()),
                        });
                        cx.notify();
                    }))
            }))
    }

    /// One side of the index boundary and the files on that side.
    fn change_section(
        &self,
        changes: &Changes,
        staged: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .bg(tokens.colors().bg_surface)
                    .child(div().flex_1().child(if staged {
                        rust_i18n::t!("surface.git.section.staged").to_string()
                    } else {
                        rust_i18n::t!("surface.git.section.unstaged").to_string()
                    }))
                    .children(
                        [(false, "surface.git.unified"), (true, "surface.git.split")].map(
                            |(split, key)| {
                                let on = self.split == split;
                                div()
                                    .id(SharedString::from(format!("diff-layout:{staged}:{split}")))
                                    .px_1p5()
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .when(on, |this| {
                                        this.bg(tokens.colors().row_active())
                                            .text_color(tokens.colors().text_primary)
                                    })
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.split = split;
                                        cx.notify();
                                    }))
                                    .child(rust_i18n::t!(key).to_string())
                            },
                        ),
                    ),
            )
            .children(
                changes
                    .files
                    .iter()
                    .filter(|file| self.diff_filter.matches(file))
                    .map(|file| self.file_row(file, staged, cx).into_any_element()),
            )
    }

    /// Remote operations stay visible even when the worktree is clean.
    fn git_remote_actions(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let history_open = self.history_open;
        let turns_open = self.turns_open;
        let checks_open = self.checks_open;
        h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_2()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                Button::new("git-sync")
                    .ghost()
                    .compact()
                    .small()
                    .icon(IconName::Redo2)
                    .label(rust_i18n::t!("surface.git.sync").to_string())
                    .tooltip(rust_i18n::t!("surface.git.sync_tooltip").to_string())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::Sync))),
            )
            .child(
                Button::new("git-pull")
                    .ghost()
                    .compact()
                    .small()
                    .label(rust_i18n::t!("surface.git.pull").to_string())
                    .tooltip(rust_i18n::t!("surface.git.pull_tooltip").to_string())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::Pull))),
            )
            .child(
                Button::new("git-push")
                    .ghost()
                    .compact()
                    .small()
                    .label(rust_i18n::t!("surface.git.push").to_string())
                    .tooltip(rust_i18n::t!("surface.git.push_tooltip").to_string())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::Push))),
            )
            .children(self.lease_push.map(|armed| {
                // Two activations, like discarding a file: overwriting a
                // remote branch is not undone from here.
                Button::new("git-push-lease")
                    .ghost()
                    .compact()
                    .small()
                    .label(if armed {
                        rust_i18n::t!("surface.git.push_lease.confirm").to_string()
                    } else {
                        rust_i18n::t!("surface.git.push_lease").to_string()
                    })
                    .tooltip(rust_i18n::t!("surface.git.push_lease_tooltip").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if armed {
                            this.lease_push = None;
                            cx.emit(SurfaceEvent::PushWithLease);
                        } else {
                            this.lease_push = Some(true);
                        }
                        cx.notify();
                    }))
            }))
            .child(
                Button::new("git-create-pr")
                    .ghost()
                    .compact()
                    .small()
                    .label(if self.opening_pull_request {
                        rust_i18n::t!("surface.git.create_pr.working").to_string()
                    } else {
                        rust_i18n::t!("surface.git.create_pr").to_string()
                    })
                    .tooltip(rust_i18n::t!("surface.git.create_pr_tooltip").to_string())
                    .disabled(self.opening_pull_request)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.opening_pull_request = true;
                        this.pull_request = None;
                        cx.emit(SurfaceEvent::CreatePullRequest);
                        cx.notify();
                    })),
            )
            .when(self.open_pull_request, |row| {
                // Orca's "Fix broken checks": what failed, and where its log
                // is, goes to the agent; nothing is sent when all is green.
                row.child(
                    Button::new("git-fix-checks")
                        .ghost()
                        .compact()
                        .small()
                        .label(rust_i18n::t!("surface.git.fix_checks").to_string())
                        .tooltip(rust_i18n::t!("surface.git.fix_checks_tooltip").to_string())
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::FixChecks))),
                )
                .child(
                    Button::new("git-checks")
                        .ghost()
                        .compact()
                        .small()
                        .selected(checks_open)
                        .label(rust_i18n::t!("surface.git.checks").to_string())
                        .tooltip(rust_i18n::t!("surface.git.checks_tooltip").to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.checks_open = !checks_open;
                            if this.checks_open {
                                this.checks = None;
                                cx.emit(SurfaceEvent::LoadChecks);
                            }
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("git-create-pr-written")
                    .ghost()
                    .compact()
                    .small()
                    .label(rust_i18n::t!("surface.git.create_pr_written").to_string())
                    .tooltip(rust_i18n::t!("surface.git.create_pr_written_tooltip").to_string())
                    .disabled(self.opening_pull_request)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.opening_pull_request = true;
                        this.pull_request = None;
                        cx.emit(SurfaceEvent::CreateGeneratedPullRequest);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("git-history")
                    .ghost()
                    .compact()
                    .small()
                    .label(rust_i18n::t!("surface.git.history").to_string())
                    .tooltip(rust_i18n::t!("surface.git.history_tooltip").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.history_open = !history_open;
                        if this.history_open {
                            cx.emit(SurfaceEvent::RefreshHistory);
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("git-turns")
                    .ghost()
                    .compact()
                    .small()
                    .label(rust_i18n::t!("surface.git.turns").to_string())
                    .tooltip(rust_i18n::t!("surface.git.turns_tooltip").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.turns_open = !turns_open;
                        cx.notify();
                    })),
            )
    }

    /// The pull request's checks, failures first, under a line that sums
    /// them up; a check with a log opens it.
    fn git_checks(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        if !(self.checks_open && self.open_pull_request) {
            return None;
        }
        let tokens = Tokens::global(cx);
        let body = v_flex()
            .id("git-checks-list")
            .w_full()
            .px_3()
            .py_1()
            .gap_0p5()
            .text_xs()
            .border_b_1()
            .border_color(tokens.colors().border_subtle);
        Some(match &self.checks {
            None => body
                .text_color(tokens.colors().text_muted)
                .child(rust_i18n::t!("surface.git.checks.loading").to_string()),
            Some(Err(error)) => body
                .text_color(tokens.colors().status_error)
                .child(error.clone()),
            Some(Ok(checks)) if checks.is_empty() => body
                .text_color(tokens.colors().text_muted)
                .child(rust_i18n::t!("surface.git.checks.none").to_string()),
            Some(Ok(checks)) => body
                .children(ginka_ui::checks::summary(checks).map(|summary| {
                    div()
                        .text_color(tokens.colors().text_secondary)
                        .child(summary)
                }))
                .children(checks.iter().enumerate().map(|(index, check)| {
                    let failed = matches!(
                        check.state,
                        ginka_protocol::model::CheckState::Failed
                            | ginka_protocol::model::CheckState::Cancelled
                    );
                    let link = check.link.clone();
                    h_flex()
                        .id(("git-check", index))
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .w(px(84.))
                                .flex_shrink_0()
                                .text_color(if failed {
                                    tokens.colors().status_error
                                } else {
                                    tokens.colors().text_muted
                                })
                                .child(ginka_ui::checks::label(check.state)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(if link.is_some() {
                                    tokens.colors().accent
                                } else {
                                    tokens.colors().text_secondary
                                })
                                .child(ginka_ui::checks::title(check)),
                        )
                        .when_some(link, |row, url| {
                            row.cursor_pointer()
                                .on_click(move |_, _, cx| cx.open_url(&url))
                        })
                })),
        })
    }

    /// Where the last pull request is, or why it could not be opened: a link
    /// to it, or git's and `gh`'s own words.
    fn pull_request_line(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let (text, link) = self.pull_request.clone()?;
        let tokens = Tokens::global(cx);
        let url = text.to_string();
        Some(
            div()
                .id("git-pull-request")
                .w_full()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(if link {
                    tokens.colors().accent
                } else {
                    tokens.colors().status_error
                })
                .when(link, |this| {
                    this.cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })
                .child(text),
        )
    }

    /// Recent commits as a graph: a lane per line of history, a dot per
    /// commit, and the curves where branches fork and merge
    /// (`ginka_ui::graph`). A row opens what that commit did.
    fn git_history(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let now = crate::daemon::now();
        let rows = ginka_ui::graph::layout(&self.history);
        let lanes = rows.iter().map(|row| row.width).max().unwrap_or(1).min(6);
        let lane_width = px(10. + lanes as f32 * GRAPH_LANE);
        // One colour a lane, from the palette the rest of the window already
        // uses, so the graph adds no colours of its own.
        let palette = [
            tokens.colors().accent,
            tokens.colors().status_attention,
            tokens.colors().status_done,
            tokens.colors().status_working,
            tokens.colors().status_error,
        ];
        let opened = self
            .commit_view
            .as_ref()
            .map(|(commit, _)| commit.id.clone());
        v_flex()
            .id("git-history")
            .w_full()
            .max_h(px(320.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .when(self.history.is_empty(), |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.history_empty").to_string()),
                )
            })
            .children(
                self.history
                    .iter()
                    .zip(rows)
                    .enumerate()
                    .map(|(index, (commit, row))| {
                        let short = commit.id.chars().take(8).collect::<String>();
                        let picked = commit.clone();
                        let on = opened.as_deref() == Some(commit.id.as_str());
                        h_flex()
                            .id(("history-row", index))
                            .w_full()
                            .h(px(GRAPH_ROW))
                            .pr_3()
                            .gap_1()
                            .items_center()
                            .cursor_pointer()
                            .when(on, |this| this.bg(tokens.colors().row_active()))
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.commit_view = Some((picked.clone(), None));
                                cx.emit(SurfaceEvent::OpenCommit(picked.clone()));
                                cx.notify();
                            }))
                            .child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, _, window, _| {
                                        paint_graph_row(&row, bounds, &palette, window);
                                    },
                                )
                                .w(lane_width)
                                .h_full()
                                .flex_shrink_0(),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(tokens.colors().text_secondary)
                                    .truncate()
                                    .child(commit.summary.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .child(format!(
                                        "{} · {}",
                                        short,
                                        ginka_ui::workspace::relative_age(now, commit.authored_at)
                                    )),
                            )
                    }),
            )
    }

    /// Completed turns are readable even after later edits or commits.
    fn git_turns(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        v_flex()
            .id("git-turns")
            .w_full()
            .max_h(px(320.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .when(
                !self.checkpoints.iter().any(|row| row.has_turn_start),
                |this| {
                    this.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("surface.git.turns_empty").to_string()),
                    )
                },
            )
            .children(
                self.checkpoints
                    .iter()
                    .filter(|checkpoint| checkpoint.has_turn_start)
                    .enumerate()
                    .map(|(index, checkpoint)| {
                        let picked = checkpoint.clone();
                        let title = rust_i18n::t!(
                            "surface.git.turn_row",
                            turn = checkpoint.turn,
                            label = checkpoint.label.clone()
                        )
                        .to_string();
                        h_flex()
                            .w_full()
                            .px_3()
                            .py_1()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new(format!("turn-row-{index}"))
                                    .ghost()
                                    .compact()
                                    .small()
                                    .label(title)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.turn_view = Some((picked.clone(), None));
                                        this.commit_view = None;
                                        cx.emit(SurfaceEvent::OpenTurn(picked.clone()));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .child(checkpoint.session.0.clone()),
                            )
                    }),
            )
    }

    /// What one commit did, read-only: its heading, the way back to the
    /// uncommitted diff, and its files with the one being read expanded.
    fn read_only_diff_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, detail, changes) = if let Some((checkpoint, changes)) = self.turn_view.clone() {
            (
                rust_i18n::t!(
                    "surface.git.turn_row",
                    turn = checkpoint.turn,
                    label = checkpoint.label
                )
                .to_string(),
                format!("{} · {}", checkpoint.session.0, checkpoint.id.0),
                changes,
            )
        } else {
            let (commit, changes) = self.commit_view.clone()?;
            (
                commit.summary,
                format!(
                    "{} · {} · {}",
                    commit.id.chars().take(12).collect::<String>(),
                    commit.author,
                    ginka_ui::workspace::relative_age(crate::daemon::now(), commit.authored_at)
                ),
                changes.map(Ok),
            )
        };
        let tokens = Tokens::global(cx).clone();
        let head = v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("commit-back")
                            .ghost()
                            .compact()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip(rust_i18n::t!("surface.git.commit_back").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.commit_view = None;
                                this.turn_view = None;
                                this.expanded = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(title),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(detail),
            )
            .child(self.diff_filter_input(cx))
            .child(self.diff_context_control(cx));
        let body: Vec<AnyElement> = match changes {
            None => vec![
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.git.reading").to_string())
                    .into_any_element(),
            ],
            Some(Err(error)) => vec![
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(tokens.colors().status_error)
                    .child(error)
                    .into_any_element(),
            ],
            Some(Ok(changes)) => {
                let visible = self.diff_filter.visible_count(&changes.files);
                let (added, removed) = changes
                    .files
                    .iter()
                    .filter(|file| self.diff_filter.matches(file))
                    .fold((0, 0), |(added, removed), file| {
                        (added + file.added, removed + file.removed)
                    });
                let mut rows = vec![
                    h_flex()
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .text_xs()
                        .child(div().flex_1().text_color(tokens.colors().text_muted).child(
                            if self.diff_filter.query.trim().is_empty() {
                                rust_i18n::t!("surface.git.summary", files = changes.files.len())
                                    .to_string()
                            } else {
                                rust_i18n::t!(
                                    "surface.git.filtered_summary",
                                    visible = visible,
                                    total = changes.files.len()
                                )
                                .to_string()
                            },
                        ))
                        .child(
                            div()
                                .text_color(tokens.colors().status_done)
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_color(tokens.colors().status_error)
                                .child(format!("-{removed}")),
                        )
                        .into_any_element(),
                ];
                if visible == 0 && !changes.files.is_empty() {
                    rows.push(
                        div()
                            .px_3()
                            .py_6()
                            .text_sm()
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("surface.git.no_matches").to_string())
                            .into_any_element(),
                    );
                }
                rows.extend(
                    changes
                        .files
                        .iter()
                        .filter(|file| self.diff_filter.matches(file))
                        .map(|file| {
                            self.file_row_read_only(file, &changes.source, cx)
                                .into_any_element()
                        }),
                );
                rows
            }
        };
        Some(
            v_flex()
                .id("commit-view")
                .flex_1()
                .overflow_y_scroll()
                .child(head)
                .children(body)
                .into_any_element(),
        )
    }

    /// The batch of comments, and the way to send it.
    ///
    /// Orca's loop, which is what the line numbers on every diff line are for:
    /// marking the three places that are wrong tells the agent more than
    /// re-prompting it from scratch, and costs the reader nothing they have not
    /// already done.
    fn review_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.comments.is_empty() {
            return None;
        }
        let tokens = Tokens::global(cx).clone();
        Some(
            h_flex()
                .w_full()
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .border_t_1()
                .border_color(tokens.colors().border_subtle)
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(tokens.colors().text_secondary)
                        .child(
                            rust_i18n::t!(
                                "surface.git.review.waiting",
                                count = self.comments.len()
                            )
                            .to_string(),
                        ),
                )
                .child(
                    div()
                        .id("send-review")
                        .px_2p5()
                        .py_1()
                        .rounded(px(tokens.radius.row))
                        .bg(tokens.colors().row_active())
                        .text_xs()
                        .text_color(tokens.colors().text_primary)
                        .cursor_pointer()
                        .hover(|this| this.bg(tokens.colors().accent.opacity(0.35)))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::SendReview)))
                        .child(rust_i18n::t!("surface.git.review.send").to_string()),
                ),
        )
    }

    /// Start a comment on a line, focused so the next keystroke lands in it.
    fn comment_on(
        &mut self,
        path: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.git.comment").to_string())
                .auto_grow(1, 4)
                .submit_on_enter(true)
        });
        let handle = state.read(cx).focus_handle(cx);
        handle.focus(window, cx);
        cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                let text = state.read(cx).value().trim().to_string();
                this.finish_comment(text, cx);
            }
        })
        .detach();
        self.commenting = Some((path, line, None, state));
        cx.notify();
    }

    /// A click on a diff line: a new comment there, or — with shift held
    /// while a comment is open on another line of the same file — that
    /// comment stretched to cover both (Orca's multi-line comments).
    fn click_line(
        &mut self,
        path: String,
        anchor: Option<u32>,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if shift
            && let Some(clicked) = anchor
            && let Some((open_path, Some(start), end, _)) = self.commenting.as_mut()
            && *open_path == path
        {
            let (low, high) = ginka_ui::comment_range::extend(*start, clicked);
            *start = low;
            *end = high;
            cx.notify();
            return;
        }
        self.comment_on(path, anchor, window, cx);
    }

    /// Hand a finished comment to the shell, which is the one holding the
    /// daemon.
    fn finish_comment(&mut self, text: String, cx: &mut Context<Self>) {
        let Some((path, line, end_line, _)) = self.commenting.take() else {
            return;
        };
        cx.notify();
        if text.is_empty() {
            return;
        }
        cx.emit(SurfaceEvent::Comment {
            path,
            line,
            end_line,
            text,
        });
    }

    /// The comments already left on a line, and the box for a new one.
    fn line_comments(
        &self,
        path: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let mut drawn: Vec<AnyElement> = self
            .comments
            .iter()
            .filter(|comment| comment.path == path && comment.line == line)
            .map(|comment| {
                let id = comment.id.clone();
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .ml_8()
                    .border_l_2()
                    .border_color(tokens.colors().accent)
                    .bg(tokens.colors().row_hover())
                    .text_xs()
                    .text_color(tokens.colors().text_primary)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(match (comment.line, comment.end_line) {
                                (Some(start), Some(end)) => format!(
                                    "{} · {}",
                                    ginka_ui::comment_range::label(start, Some(end)),
                                    comment.text
                                ),
                                _ => comment.text.clone(),
                            }),
                    )
                    .child(
                        // Taken back before it is sent: a comment that turned
                        // out wrong should not reach the agent.
                        Button::new(SharedString::from(format!("remove-comment:{}", comment.id)))
                            .ghost()
                            .compact()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip(rust_i18n::t!("surface.git.comment_remove").to_string())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SurfaceEvent::RemoveComment { id: id.clone() })
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        if let Some((open_path, open_line, end_line, state)) = &self.commenting
            && open_path == path
            && *open_line == line
        {
            drawn.push(
                v_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .ml_8()
                    .gap_0p5()
                    .border_l_2()
                    .border_color(tokens.colors().accent)
                    // Which lines it is about, and how to make it more.
                    .children(open_line.map(|start| {
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .child(
                                rust_i18n::t!(
                                    "surface.git.comment_lines",
                                    lines = ginka_ui::comment_range::label(start, *end_line)
                                )
                                .to_string(),
                            )
                    }))
                    .child(Textarea::new(state))
                    .into_any_element(),
            );
        }
        drawn
    }

    /// Where the reviewed work is written up and sent.
    ///
    /// Under the files rather than above them: the message is what you write
    /// once you have read them, and a box at the top invites writing it first.
    fn commit_box(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let selected = self.commit_selection.active();
        let count = self.commit_selection.count(self.commit_files());
        let blocked = self.generation.active() || (selected && count == 0);
        let amendable = !selected && self.history.first().is_some_and(|head| !head.published);

        v_flex()
            .w_full()
            .p_2()
            .gap_1p5()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
            .when(selected, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(tokens.colors().text_secondary)
                                        .child(
                                            rust_i18n::t!(
                                                "surface.git.selected_files",
                                                count = count
                                            )
                                            .to_string(),
                                        ),
                                )
                                .child(
                                    Button::new("clear-commit-selection")
                                        .ghost()
                                        .compact()
                                        .small()
                                        .disabled(self.generation.active())
                                        .label(
                                            rust_i18n::t!("surface.git.clear_selection")
                                                .to_string(),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.commit_selection.clear();
                                            this.complaint = None;
                                            this.commit_refused = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .child(rust_i18n::t!("surface.git.selection_hint").to_string()),
                        ),
                )
            })
            .children(self.complaint.clone().map(|why| {
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .rounded(px(tokens.radius.row))
                    .text_xs()
                    .text_color(tokens.colors().status_error)
                    .child(why)
            }))
            .when(self.commit_refused && !selected, |this| {
                this.child(
                    Button::new("commit-fix-with-agent")
                        .ghost()
                        .compact()
                        .small()
                        .label(rust_i18n::t!("surface.git.fix_commit").to_string())
                        .tooltip(rust_i18n::t!("surface.git.fix_commit_tooltip").to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let Some(output) = this.complaint.clone() else {
                                return;
                            };
                            let message = this
                                .message
                                .as_ref()
                                .map(|state| state.read(cx).value().to_string())
                                .unwrap_or_default();
                            this.commit_refused = false;
                            this.complaint = None;
                            cx.emit(SurfaceEvent::FixCommit {
                                message,
                                output: output.to_string(),
                            });
                            cx.notify();
                        })),
                )
            })
            .child(match self.message.clone() {
                Some(state) => v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(
                        div()
                            .w_full()
                            .px_2()
                            .py_1p5()
                            .rounded(px(tokens.radius.panel))
                            .bg(tokens.colors().bg_surface)
                            .border_1()
                            .border_color(tokens.colors().border_subtle)
                            .child(Textarea::new(&state)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                Button::new("commit")
                                    .primary()
                                    .compact()
                                    .small()
                                    .disabled(blocked)
                                    .label(rust_i18n::t!("surface.git.commit").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.commit(CommitThen::Nothing, false, cx)
                                    })),
                            )
                            .child(
                                Button::new("commit-push")
                                    .ghost()
                                    .compact()
                                    .small()
                                    .disabled(blocked)
                                    .label(rust_i18n::t!("surface.git.commit_push").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.commit(CommitThen::Push, false, cx)
                                    })),
                            )
                            .child(
                                Button::new("commit-pr")
                                    .ghost()
                                    .compact()
                                    .small()
                                    .disabled(blocked)
                                    .label(rust_i18n::t!("surface.git.commit_pr").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.commit(CommitThen::PullRequest, false, cx)
                                    })),
                            )
                            .when(amendable, |row| {
                                row.child(
                                    Button::new("commit-amend")
                                        .ghost()
                                        .compact()
                                        .small()
                                        .disabled(blocked)
                                        .label(rust_i18n::t!("surface.git.amend").to_string())
                                        .tooltip(
                                            rust_i18n::t!("surface.git.amend_hint").to_string(),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.commit(CommitThen::Nothing, true, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("generate-commit")
                                    .ghost()
                                    .compact()
                                    .small()
                                    .disabled(blocked)
                                    .label(if self.generation.active() {
                                        rust_i18n::t!("surface.git.generating").to_string()
                                    } else {
                                        rust_i18n::t!("surface.git.generate").to_string()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.generation.active() || !this.valid_commit_scope(cx)
                                        {
                                            return;
                                        }
                                        let generation_id = uuid::Uuid::new_v4().to_string();
                                        this.generation.start(generation_id.clone());
                                        this.complaint = None;
                                        cx.emit(SurfaceEvent::GenerateCommitMessage {
                                            generation_id,
                                            only_staged: this.only_staged(),
                                            paths: this.commit_selection.paths(this.commit_files()),
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("cancel-commit")
                                    .ghost()
                                    .compact()
                                    .small()
                                    .label(rust_i18n::t!("surface.git.cancel").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.message = None;
                                        this.complaint = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element(),
                None => Button::new("write-commit")
                    .ghost()
                    .small()
                    .label(rust_i18n::t!("surface.git.write_commit").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.write_commit(window, cx)))
                    .into_any_element(),
            })
    }

    /// Open the message box, focused, so the next keystroke lands in it.
    fn write_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.git.message").to_string())
                .auto_grow(1, 6)
        });
        let handle = state.read(cx).focus_handle(cx);
        handle.focus(window, cx);
        self.message = Some(state);
        self.complaint = None;
        cx.notify();
    }

    /// Hand the message to the shell, which is the one holding the daemon.
    fn commit(&mut self, then: CommitThen, amend: bool, cx: &mut Context<Self>) {
        if self.generation.active()
            || (amend && self.commit_selection.active())
            || !self.valid_commit_scope(cx)
        {
            return;
        }
        let Some(state) = self.message.as_ref() else {
            return;
        };
        let message = state.read(cx).value().trim().to_string();
        if message.is_empty() && !amend {
            self.complaint = Some(
                rust_i18n::t!("surface.git.needs_message")
                    .to_string()
                    .into(),
            );
            cx.notify();
            return;
        }
        if matches!(then, CommitThen::PullRequest) {
            self.opening_pull_request = true;
            self.pull_request = None;
        }
        cx.emit(SurfaceEvent::Commit {
            message,
            only_staged: self.only_staged(),
            then,
            amend,
            paths: self.commit_selection.paths(self.commit_files()),
        });
    }

    /// What can be done to one file: put it in the next commit, or undo it.
    ///
    /// On the row rather than behind a menu, because both answers are ones a
    /// reader reaches for while reading, and a menu is a second decision about
    /// where the first one lives.
    fn file_actions(
        &self,
        file: &ginka_protocol::model::FileChange,
        staged: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let asking = self.reverting.as_deref() == Some(file.path.as_str());
        let path = file.path.clone();
        let to_stage = path.clone();
        let to_revert = path.clone();
        let to_arm = path.clone();

        let mut actions: Vec<AnyElement> = vec![
            div()
                .id(SharedString::from(format!("stage:{staged}:{path}")))
                .px(px(7.))
                .py(px(2.))
                .rounded(px(tokens.radius.row))
                .text_xs()
                .when(staged, |this| this.bg(tokens.colors().row_active()))
                .text_color(if staged {
                    tokens.colors().text_primary
                } else {
                    tokens.colors().text_muted.opacity(0.7)
                })
                .cursor_pointer()
                .hover(|this| this.bg(tokens.colors().row_hover()))
                // The click belongs to the control, not to the row it sits on:
                // staging a file must not also collapse its diff.
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(SurfaceEvent::Stage {
                        path: to_stage.clone(),
                        staged: !staged,
                    });
                }))
                .child(if staged {
                    rust_i18n::t!("surface.git.staged").to_string()
                } else {
                    rust_i18n::t!("surface.git.stage").to_string()
                })
                .into_any_element(),
        ];

        if asking && !staged {
            actions.push(
                div()
                    .id(SharedString::from(format!("revert-yes:{path}")))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(tokens.radius.row))
                    .bg(tokens.colors().status_error.opacity(0.22))
                    .text_xs()
                    .text_color(tokens.colors().text_primary)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.reverting = None;
                        cx.emit(SurfaceEvent::Revert {
                            path: to_revert.clone(),
                        });
                    }))
                    .child(rust_i18n::t!("surface.git.revert.yes").to_string())
                    .into_any_element(),
            );
        }
        if !staged {
            actions.push(
                div()
                    .id(SharedString::from(format!("revert:{path}")))
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.reverting = (!asking).then(|| to_arm.clone());
                        cx.notify();
                    }))
                    .child(if asking {
                        rust_i18n::t!("surface.git.revert.no").to_string()
                    } else {
                        rust_i18n::t!("surface.git.revert").to_string()
                    })
                    .into_any_element(),
            );
        }
        actions
    }

    /// One file, and its diff when it is the one being read.
    /// One file of a commit: history is read, not staged or reverted, so the
    /// row opens its diff and nothing else.
    fn file_row_read_only(
        &self,
        file: &ginka_protocol::model::FileChange,
        source: &ginka_protocol::model::ChangeSource,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let image_view = (file.binary && self.expanded.as_deref() == Some(file.path.as_str()))
            .then(|| self.image_diff_view(file, source, cx));
        let tokens = Tokens::global(cx).clone();
        let expanded = self.expanded.as_deref() == Some(file.path.as_str());
        let path = file.path.clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .id(SharedString::from(format!("commit-file:{}", file.path)))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(expanded, |this| this.bg(tokens.colors().row_active()))
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded = (!expanded).then(|| path.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(26.))
                            .text_xs()
                            .text_color(match file.kind {
                                ChangeKind::Added => tokens.colors().status_done,
                                ChangeKind::Deleted => tokens.colors().status_error,
                                _ => tokens.colors().text_muted,
                            })
                            .child(match file.kind {
                                ChangeKind::Added => "A",
                                ChangeKind::Modified => "M",
                                ChangeKind::Deleted => "D",
                                ChangeKind::Renamed => "R",
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(tokens.colors().text_secondary)
                            .truncate()
                            .child(file.label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_done)
                            .child(format!("+{}", file.added)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(format!("-{}", file.removed)),
                    ),
            )
            .children(image_view)
            .when(expanded && !file.binary, |this| {
                this.children(file.hunks.iter().map(|hunk| {
                    v_flex()
                        .w_full()
                        .child(
                            div()
                                .w_full()
                                .px_3()
                                .py_0p5()
                                .bg(tokens.colors().bg_surface)
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .child(hunk.header.clone()),
                        )
                        .children(hunk.lines.iter().map(|line| {
                            h_flex()
                                .w_full()
                                .px_3()
                                .gap_2()
                                .font_family(mono.clone())
                                .text_xs()
                                .line_height(px(17.))
                                .when(line.kind == LineKind::Added, |this| {
                                    this.bg(tokens.colors().status_done.opacity(0.10))
                                })
                                .when(line.kind == LineKind::Removed, |this| {
                                    this.bg(tokens.colors().status_error.opacity(0.10))
                                })
                                .child(
                                    div()
                                        .w(px(34.))
                                        .text_color(tokens.colors().text_muted.opacity(0.7))
                                        .child(
                                            match line.kind {
                                                LineKind::Removed => line.old_line,
                                                _ => line.new_line,
                                            }
                                            .map(|at| at.to_string())
                                            .unwrap_or_default(),
                                        ),
                                )
                                .child(diff_text(line, &tokens))
                        }))
                }))
            })
    }

    fn file_row(
        &self,
        file: &ginka_protocol::model::FileChange,
        staged: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let row_key = format!(
            "{}:{}",
            if staged { "staged" } else { "unstaged" },
            file.path
        );
        let expanded = self.expanded.as_deref() == Some(row_key.as_str());
        let expand_key = row_key.clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();

        let selected_path = file.path.clone();
        let selection = Checkbox::new(SharedString::from(format!("commit-select:{row_key}")))
            .small()
            .checked(self.commit_selection.contains(&file.path))
            .disabled(self.generation.active())
            .accessibility_label(
                rust_i18n::t!("surface.git.select_file", path = file.path.as_str()).to_string(),
            )
            .tooltip(rust_i18n::t!("surface.git.selection_hint").to_string())
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                cx.stop_propagation();
                this.commit_selection.set(selected_path.clone(), *checked);
                this.complaint = None;
                this.commit_refused = false;
                cx.notify();
            }));

        v_flex()
            .w_full()
            .child(
                h_flex()
                    .id(SharedString::from(format!("file:{row_key}")))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .when(expanded, |this| this.bg(tokens.colors().row_active()))
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded = (!expanded).then(|| expand_key.clone());
                        cx.notify();
                    }))
                    .child(selection)
                    .child(
                        div()
                            .w(px(26.))
                            .text_xs()
                            .text_color(match file.kind {
                                ChangeKind::Added => tokens.colors().status_done,
                                ChangeKind::Deleted => tokens.colors().status_error,
                                _ => tokens.colors().text_muted,
                            })
                            .child(match file.kind {
                                ChangeKind::Added => "A",
                                ChangeKind::Modified => "M",
                                ChangeKind::Deleted => "D",
                                ChangeKind::Renamed => "R",
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(tokens.colors().text_secondary)
                            // A path's meaning is in its tail.
                            .truncate()
                            .child(file.label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_done)
                            .child(format!("+{}", file.added)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(format!("-{}", file.removed)),
                    )
                    .children(self.file_actions(file, staged, cx)),
            )
            .when(expanded && file.binary, |this| {
                let source = if staged {
                    ginka_protocol::model::ChangeSource::Staged
                } else {
                    ginka_protocol::model::ChangeSource::Unstaged
                };
                this.child(self.image_diff_view(file, &source, cx))
            })
            .when(expanded && !file.binary, |this| {
                this.children(file.hunks.iter().map(|hunk| {
                    let hunk_path = file.path.clone();
                    let hunk_header = hunk.header.clone();
                    let action_header = hunk_header.clone();
                    let discard_path = file.path.clone();
                    let discard_header = hunk.header.clone();
                    let discard_key = format!("{}\0{}", file.path, hunk.header);
                    let discard_armed = self.reverting_hunk.as_deref() == Some(&discard_key);
                    v_flex()
                        .w_full()
                        .child(
                            h_flex()
                                .w_full()
                                .px_3()
                                .py_0p5()
                                .bg(tokens.colors().bg_surface)
                                .child(
                                    div()
                                        .flex_1()
                                        .font_family(mono.clone())
                                        .text_xs()
                                        .text_color(tokens.colors().text_muted)
                                        .child(hunk_header),
                                )
                                .when(self.diff_context == 3, |this| {
                                    this.child(
                                        Button::new(format!(
                                            "hunk:{}:{}:{}",
                                            if staged { "unstage" } else { "stage" },
                                            file.path,
                                            hunk.header
                                        ))
                                        .ghost()
                                        .compact()
                                        .small()
                                        .label(if staged {
                                            rust_i18n::t!("surface.git.hunk.unstage").to_string()
                                        } else {
                                            rust_i18n::t!("surface.git.hunk.stage").to_string()
                                        })
                                        .on_click(
                                            cx.listener(move |_, _, _, cx| {
                                                cx.stop_propagation();
                                                cx.emit(SurfaceEvent::StageHunk {
                                                    path: hunk_path.clone(),
                                                    header: action_header.clone(),
                                                    staged: !staged,
                                                });
                                            }),
                                        ),
                                    )
                                })
                                .when(!staged && self.diff_context == 3, |this| {
                                    this.child(
                                        Button::new(format!(
                                            "discard-hunk:{}:{}",
                                            file.path, hunk.header
                                        ))
                                        .ghost()
                                        .compact()
                                        .small()
                                        .label(if discard_armed {
                                            rust_i18n::t!("surface.git.hunk.discard_confirm")
                                                .to_string()
                                        } else {
                                            rust_i18n::t!("surface.git.hunk.discard").to_string()
                                        })
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                if discard_armed {
                                                    this.reverting_hunk = None;
                                                    cx.emit(SurfaceEvent::RevertHunk {
                                                        path: discard_path.clone(),
                                                        header: discard_header.clone(),
                                                    });
                                                } else {
                                                    this.reverting_hunk = Some(discard_key.clone());
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    )
                                }),
                        )
                        .when(self.split, |this| {
                            this.children(
                                ginka_ui::split_diff::rows(hunk).into_iter().map(|row| {
                                    split_row(&file.path, row, &tokens, mono.clone(), cx)
                                }),
                            )
                        })
                        .children(
                            (!self.split)
                                .then(|| hunk.lines.iter())
                                .into_iter()
                                .flatten()
                                .map(|line| {
                                    let anchor = match line.kind {
                                        LineKind::Removed => line.old_line,
                                        _ => line.new_line,
                                    };
                                    let path = file.path.clone();
                                    h_flex()
                                        .id(SharedString::from(format!(
                                            "line:{}:{:?}:{:?}",
                                            file.path, line.kind, anchor
                                        )))
                                        .w_full()
                                        .px_3()
                                        .gap_2()
                                        .cursor_pointer()
                                        .hover(|this| this.bg(tokens.colors().row_hover()))
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, window, cx| {
                                                this.click_line(
                                                    path.clone(),
                                                    anchor,
                                                    event.modifiers().shift,
                                                    window,
                                                    cx,
                                                )
                                            },
                                        ))
                                        .font_family(mono.clone())
                                        .text_xs()
                                        .line_height(px(17.))
                                        .when(line.kind == LineKind::Added, |this| {
                                            this.bg(tokens.colors().status_done.opacity(0.10))
                                        })
                                        .when(line.kind == LineKind::Removed, |this| {
                                            this.bg(tokens.colors().status_error.opacity(0.10))
                                        })
                                        .child(
                                            div()
                                                .w(px(34.))
                                                .text_color(tokens.colors().text_muted.opacity(0.7))
                                                .child(match line.kind {
                                                    // The number a comment would be
                                                    // anchored to: the new side for
                                                    // anything that still exists.
                                                    LineKind::Removed => line
                                                        .old_line
                                                        .map(|at| at.to_string())
                                                        .unwrap_or_default(),
                                                    _ => line
                                                        .new_line
                                                        .map(|at| at.to_string())
                                                        .unwrap_or_default(),
                                                }),
                                        )
                                        // Orca's line attribution: a glyph,
                                        // named in its tooltip, beside what
                                        // an agent's turn wrote.
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "by-agent:{}:{:?}",
                                                    file.path, line.new_line
                                                )))
                                                .w(px(12.))
                                                .flex_shrink_0()
                                                .text_color(tokens.colors().accent)
                                                .when(line.by_agent == Some(true), |this| {
                                                    this.child("✦").tooltip(|window, cx| {
                                                        Tooltip::new(
                                                            rust_i18n::t!("surface.git.by_agent")
                                                                .to_string(),
                                                        )
                                                        .build(window, cx)
                                                    })
                                                }),
                                        )
                                        .child(diff_text(line, &tokens))
                                        .into_any_element()
                                }),
                        )
                        .children(hunk.lines.iter().flat_map(|line| {
                            let anchor = match line.kind {
                                LineKind::Removed => line.old_line,
                                _ => line.new_line,
                            };
                            self.line_comments(&file.path, anchor, cx)
                        }))
                }))
            })
    }

    fn placeholder(&self, surface: Surface, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                surface
                    .icon()
                    .size_6()
                    .text_color(tokens.colors().text_muted),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.colors().text_secondary)
                    .child(surface.title()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(surface.availability()),
            )
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn browser(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let capture = self.browser_capture.clone();
        let capture_card = capture.map(|capture| {
            let label = if capture
                .element
                .accessibility_name
                .as_deref()
                .is_some_and(|name| !name.is_empty())
            {
                capture
                    .element
                    .accessibility_name
                    .clone()
                    .unwrap_or_default()
            } else if !capture.element.text.is_empty() {
                capture.element.text.clone()
            } else {
                capture.element.selector.clone()
            };
            v_flex()
                .w_full()
                .flex_shrink_0()
                .gap_2()
                .p_3()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .bg(tokens.colors().bg_surface)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(tokens.colors().text_primary)
                                .child(format!("<{}> {label}", capture.element.tag_name)),
                        )
                        .child(
                            Button::new("browser-capture-dismiss")
                                .ghost()
                                .compact()
                                .small()
                                .icon(IconName::Close)
                                .tooltip(rust_i18n::t!("surface.browser.dismiss").to_string())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.browser_capture = None;
                                    this.browser_feedback.update(cx, |feedback, cx| {
                                        feedback.set_value("", window, cx)
                                    });
                                    cx.notify();
                                })),
                        ),
                )
                .child(Textarea::new(&self.browser_feedback).h(px(70.)))
                .child(
                    h_flex().justify_end().child(
                        Button::new("browser-capture-send")
                            .primary()
                            .label(rust_i18n::t!("surface.browser.add_to_chat").to_string())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let feedback = this.browser_feedback.read(cx).value().to_string();
                                let context =
                                    ginka_core::browser::format_capture(&capture, &feedback);
                                this.browser_capture = None;
                                this.browser_feedback
                                    .update(cx, |feedback, cx| feedback.set_value("", window, cx));
                                cx.emit(SurfaceEvent::AddBrowserContext(context));
                                cx.notify();
                            })),
                    ),
                )
        });
        let Some(workspace) = self.browser_workspace.as_ref() else {
            return self.placeholder(Surface::Browser, cx).into_any_element();
        };
        let Some(browser) = self.browser_tabs.get(workspace) else {
            return self.placeholder(Surface::Browser, cx).into_any_element();
        };
        match browser {
            Ok(browser) => v_flex()
                .flex_1()
                .min_h_0()
                .children(capture_card)
                .child(browser.clone())
                .into_any_element(),
            Err(error) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(tokens.colors().status_error)
                .child(error.clone())
                .into_any_element(),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn browser(&self, cx: &mut Context<Self>) -> AnyElement {
        self.placeholder(Surface::Browser, cx).into_any_element()
    }
}

impl SurfacePanel {
    /// The Reports surface: what the work cost, by day, by agent and by
    /// login, and each login's rate-limit windows with the reading's age.
    ///
    /// Lists, not charts: the question mid-task is "how close am I to the
    /// wall" (roadmap N12), and a number with its age answers it where a bar
    /// would only decorate it. The percentage is printed and *at the wall*
    /// is a word beside it, never a colour alone (§6.4).
    fn reports(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let now = crate::daemon::now();
        let Some(usage) = self.usage.as_ref() else {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.reports.reading").to_string()),
                )
                .into_any_element();
        };
        if usage.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.reports.empty").to_string()),
                )
                .into_any_element();
        }

        let heading = |text: String| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .text_color(tokens.colors().text_muted)
                .child(text)
        };
        let row = |label: String, detail: String| {
            h_flex()
                .w_full()
                .px_3()
                .py_1()
                .gap_3()
                .items_baseline()
                .child(
                    div()
                        .w(px(120.))
                        .flex_shrink_1()
                        .min_w(px(64.))
                        .text_sm()
                        .text_color(tokens.colors().text_primary)
                        .truncate()
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(tokens.colors().text_secondary)
                        .child(detail),
                )
        };
        let usage_rows = |rows: &[ginka_protocol::model::UsageRow]| {
            rows.iter()
                .map(|entry| {
                    row(
                        entry.label.clone(),
                        ginka_ui::reports::totals_line(&entry.totals),
                    )
                    .into_any_element()
                })
                .collect::<Vec<_>>()
        };
        let at_the_wall = rust_i18n::t!("composer.account.at_wall").to_string();
        let plan_rows: Vec<AnyElement> = usage
            .plans
            .iter()
            .map(|snapshot| {
                let mut lines = ginka_ui::reports::window_lines(snapshot, now, &at_the_wall);
                if let Some(plan) = &snapshot.usage.plan {
                    lines.insert(0, plan.clone());
                }
                lines.push(
                    rust_i18n::t!(
                        "composer.account.age",
                        age = ginka_ui::workspace::relative_age(now, snapshot.observed_at)
                    )
                    .to_string(),
                );
                row(snapshot.account.0.clone(), lines.join(" · ")).into_any_element()
            })
            .collect();

        v_flex()
            .id("reports")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb_3()
            .child(heading(
                rust_i18n::t!("surface.reports.windows").to_string(),
            ))
            .children(plan_rows)
            .children((usage.plans.is_empty()).then(|| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.reports.no_reading").to_string())
            }))
            .child(heading(
                rust_i18n::t!("surface.reports.by_account").to_string(),
            ))
            .children(usage_rows(&usage.by_account))
            .child(heading(
                rust_i18n::t!("surface.reports.by_agent").to_string(),
            ))
            .children(usage_rows(&usage.by_agent))
            .child(heading(
                rust_i18n::t!("surface.reports.by_project").to_string(),
            ))
            .children(usage_rows(&usage.by_project))
            .child(heading(
                rust_i18n::t!("surface.reports.by_model").to_string(),
            ))
            .children(usage_rows(&usage.by_model))
            .child(heading(rust_i18n::t!("surface.reports.by_day").to_string()))
            .children(usage_rows(&usage.by_day))
            // Where the estimates came from, so a `≈` is never unexplained.
            .child(
                div()
                    .px_3()
                    .pt_3()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(match usage.rates_fetched_at {
                        Some(at) => rust_i18n::t!(
                            "surface.reports.rates",
                            age = ginka_ui::workspace::relative_age(now, at)
                        )
                        .to_string(),
                        None => rust_i18n::t!("surface.reports.no_rates").to_string(),
                    }),
            )
            .into_any_element()
    }

    /// The agents' own skills, grouped across the places each was installed.
    fn skills(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let changing = self.skill_changing.clone();
        let Some(skills) = self.skills.clone() else {
            let message = self
                .skill_error
                .clone()
                .unwrap_or_else(|| rust_i18n::t!("surface.skills.reading").to_string().into());
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(if self.skill_error.is_some() {
                            tokens.colors().status_error
                        } else {
                            tokens.colors().text_muted
                        })
                        .child(message),
                )
                .children(self.skill_error.as_ref().map(|_| {
                    Button::new("retry-skills")
                        .ghost()
                        .child(rust_i18n::t!("surface.skills.retry").to_string())
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SurfaceEvent::RefreshSkills)))
                }))
                .into_any_element();
        };

        let total = skills.len();
        let visible = ginka_ui::skills::filter_skills(&skills, &self.skill_filter)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let visible_count = visible.len();
        let scope = self.skill_filter.scope;
        let state = self.skill_filter.state;
        let active_bg = tokens.colors().row_active();
        let local_paths = self.local_paths;

        let rows = visible.into_iter().map(|skill| {
            let request = ginka_ui::skills::toggle_request(&skill);
            let name = request.name.clone();
            let enabled = request.enabled;
            let is_changing = changing.as_deref() == Some(name.as_str());
            let installs = ginka_ui::skills::install_rows(&skill)
                .iter()
                .map(|install| {
                    let path = ginka_ui::skills::install_path_text(install);
                    let copied_path = path.clone();
                    let directory_url =
                        ginka_ui::skills::install_directory_url(install, local_paths);
                    let scope = match install.scope {
                        SkillScope::User => rust_i18n::t!("surface.skills.scope.user").to_string(),
                        SkillScope::Project => {
                            rust_i18n::t!("surface.skills.scope.project").to_string()
                        }
                    };
                    let state = if install.enabled {
                        rust_i18n::t!("surface.skills.enabled").to_string()
                    } else {
                        rust_i18n::t!("surface.skills.disabled").to_string()
                    };
                    v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .text_xs()
                                .text_color(tokens.colors().text_secondary)
                                .child(format!("{} · {scope} · {state}", install.root_label)),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(10.))
                                        .text_color(tokens.colors().text_muted)
                                        .truncate()
                                        .child(path),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "copy-skill-path:{}:{}",
                                        skill.name, install.root_label
                                    )))
                                    .ghost()
                                    .child(rust_i18n::t!("surface.skills.copy_path").to_string())
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copied_path.clone(),
                                            ));
                                        },
                                    ),
                                )
                                .children(directory_url.map(|url| {
                                    Button::new(SharedString::from(format!(
                                        "open-skill-folder:{}:{}",
                                        skill.name, install.root_label
                                    )))
                                    .ghost()
                                    .child(rust_i18n::t!("surface.skills.open_folder").to_string())
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                                })),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            let action = if is_changing {
                rust_i18n::t!("surface.skills.changing").to_string()
            } else if enabled {
                rust_i18n::t!("surface.skills.enable").to_string()
            } else {
                rust_i18n::t!("surface.skills.disable").to_string()
            };
            v_flex()
                .w_full()
                .px_3()
                .py_2p5()
                .gap_2()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(tokens.colors().text_primary)
                                        .child(skill.name),
                                )
                                .children(skill.description.map(|description| {
                                    div()
                                        .text_xs()
                                        .text_color(tokens.colors().text_secondary)
                                        .child(description)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("toggle-skill:{name}")))
                                .ghost()
                                .disabled(changing.is_some())
                                .child(action)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(SurfaceEvent::SetSkillEnabled {
                                        name: name.clone(),
                                        enabled,
                                    })
                                })),
                        ),
                )
                .child(v_flex().gap_1().children(installs))
                .into_any_element()
        });

        v_flex()
            .id("skills-surface")
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .w_full()
                    .p_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(ginka_ui::field::input(&self.skill_name))
                            .child(ginka_ui::field::input(&self.skill_description))
                            .child(Textarea::new(&self.skill_body).h(px(80.)))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("skill-create-user")
                                            .ghost()
                                            .when(!self.skill_create_project, |this| {
                                                this.bg(active_bg)
                                            })
                                            .child(format!(
                                                "{}{}",
                                                if self.skill_create_project {
                                                    ""
                                                } else {
                                                    "✓ "
                                                },
                                                rust_i18n::t!("surface.skills.scope.user")
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.skill_create_project = false;
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.skill_project_available, |this| {
                                        this.child(
                                            Button::new("skill-create-project")
                                                .ghost()
                                                .when(self.skill_create_project, |this| {
                                                    this.bg(active_bg)
                                                })
                                                .child(format!(
                                                    "{}{}",
                                                    if self.skill_create_project {
                                                        "✓ "
                                                    } else {
                                                        ""
                                                    },
                                                    rust_i18n::t!("surface.skills.scope.project")
                                                ))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.skill_create_project = true;
                                                    cx.notify();
                                                })),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        Button::new("skill-create")
                                            .disabled(self.skill_creating)
                                            .child(
                                                rust_i18n::t!("surface.skills.create.action")
                                                    .to_string(),
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.emit(SurfaceEvent::CreateSkill {
                                                    name: this
                                                        .skill_name
                                                        .read(cx)
                                                        .value()
                                                        .to_string(),
                                                    description: this
                                                        .skill_description
                                                        .read(cx)
                                                        .value()
                                                        .to_string(),
                                                    body: this
                                                        .skill_body
                                                        .read(cx)
                                                        .value()
                                                        .to_string(),
                                                    project: this.skill_create_project,
                                                });
                                            })),
                                    ),
                            ),
                    )
                    .child(ginka_ui::field::input(&self.skill_finder))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                Button::new("skill-scope-all")
                                    .ghost()
                                    .when(scope == ScopeFilter::All, |this| this.bg(active_bg))
                                    .child(rust_i18n::t!("surface.skills.filter.all").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.skill_filter.scope = ScopeFilter::All;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("skill-scope-project")
                                    .ghost()
                                    .when(scope == ScopeFilter::Project, |this| this.bg(active_bg))
                                    .child(
                                        rust_i18n::t!("surface.skills.scope.project").to_string(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.skill_filter.scope = ScopeFilter::Project;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("skill-scope-user")
                                    .ghost()
                                    .when(scope == ScopeFilter::User, |this| this.bg(active_bg))
                                    .child(rust_i18n::t!("surface.skills.scope.user").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.skill_filter.scope = ScopeFilter::User;
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .child(
                                        rust_i18n::t!(
                                            "surface.skills.filter.count",
                                            visible = visible_count,
                                            total = total
                                        )
                                        .to_string(),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                Button::new("skill-state-all")
                                    .ghost()
                                    .when(state == StateFilter::All, |this| this.bg(active_bg))
                                    .child(
                                        rust_i18n::t!("surface.skills.filter.any_state")
                                            .to_string(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.skill_filter.state = StateFilter::All;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("skill-state-enabled")
                                    .ghost()
                                    .when(state == StateFilter::Enabled, |this| this.bg(active_bg))
                                    .child(rust_i18n::t!("surface.skills.enabled").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.skill_filter.state = StateFilter::Enabled;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("skill-state-disabled")
                                    .ghost()
                                    .when(state == StateFilter::Disabled, |this| this.bg(active_bg))
                                    .child(rust_i18n::t!("surface.skills.disabled").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.skill_filter.state = StateFilter::Disabled;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("skills-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(self.skill_error.clone().map(|error| {
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(
                                rust_i18n::t!("surface.skills.error", error = error.as_ref())
                                    .to_string(),
                            )
                    }))
                    .children(self.skills_truncated.then(|| {
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(tokens.colors().status_error)
                            .child(rust_i18n::t!("surface.skills.truncated").to_string())
                    }))
                    .children((visible_count == 0).then(|| {
                        div()
                            .w_full()
                            .px_3()
                            .py_6()
                            .text_sm()
                            .text_color(tokens.colors().text_muted)
                            .child(if total == 0 {
                                rust_i18n::t!("surface.skills.empty").to_string()
                            } else {
                                rust_i18n::t!("surface.skills.no_matches").to_string()
                            })
                    }))
                    .children(rows),
            )
            .into_any_element()
    }

    /// The files surface: a finder over the worktree, and what it opens.
    ///
    fn files(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .w_full()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(ginka_ui::field::input(&self.finder))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("file-search-workspace")
                                    .ghost()
                                    .when(
                                        self.file_search_scope == FileSearchScope::Workspace,
                                        |this| this.bg(tokens.colors().row_hover()),
                                    )
                                    .child(
                                        rust_i18n::t!("surface.files.scope.workspace").to_string(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_file_search_scope(FileSearchScope::Workspace, cx)
                                    })),
                            )
                            .child(
                                Button::new("file-search-project")
                                    .ghost()
                                    .when(
                                        self.file_search_scope == FileSearchScope::Project,
                                        |this| this.bg(tokens.colors().row_hover()),
                                    )
                                    .child(rust_i18n::t!("surface.files.scope.project").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_file_search_scope(FileSearchScope::Project, cx)
                                    })),
                            ),
                    ),
            )
            .children(
                (!self.file_tabs.paths().is_empty()
                    || self.file_tabs.can_go_back()
                    || self.file_tabs.can_go_forward())
                .then(|| self.file_tab_strip(cx)),
            )
            .child(match (self.browsing_files, self.active_file()) {
                (false, Some(buffer)) => self.file_view(buffer, mono, cx).into_any_element(),
                _ => self.file_list(cx).into_any_element(),
            })
    }

    fn file_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let active = self.file_tabs.active().map(str::to_owned);
        h_flex()
            .id("file-tabs")
            .w_full()
            .flex_shrink_0()
            .overflow_x_scroll()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                Button::new("file-history-back")
                    .label(rust_i18n::t!("surface.files.history.back").to_string())
                    .ghost()
                    .compact()
                    .small()
                    .disabled(!self.file_tabs.can_go_back())
                    .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
            )
            .child(
                Button::new("file-history-forward")
                    .label(rust_i18n::t!("surface.files.history.forward").to_string())
                    .ghost()
                    .compact()
                    .small()
                    .disabled(!self.file_tabs.can_go_forward())
                    .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))),
            )
            .child(
                Button::new("browse-files")
                    .label(rust_i18n::t!("surface.files.browse").to_string())
                    .ghost()
                    .compact()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.browse_files(cx))),
            )
            .children(self.file_tabs.paths().iter().map(|path| {
                let focusing = path.clone();
                let closing = path.clone();
                let label = std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string();
                let showing = active.as_deref() == Some(path);
                // A preview tab is said in words, not only in italics: the
                // next single click replaces it.
                let label = if self.file_tabs.is_preview(path) {
                    rust_i18n::t!("surface.files.preview_tab", name = label).to_string()
                } else {
                    label
                };
                ginka_ui::chrome::tab(SharedString::from(format!("file-tab:{path}")), showing, cx)
                    .gap_0p5()
                    .child(
                        Button::new(SharedString::from(format!("focus-file-tab:{path}")))
                            .text()
                            .xsmall()
                            .max_w(px(150.))
                            .label(label)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.focus_file(&focusing, cx)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("close-file-tab:{path}")))
                            .ghost()
                            .xsmall()
                            .accessibility_label(
                                rust_i18n::t!("surface.files.close", path = path.clone())
                                    .to_string(),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_file(&closing, cx);
                            }))
                            .child(
                                Icon::new(IconName::Close)
                                    .size(px(11.))
                                    .text_color(tokens.colors().text_muted),
                            ),
                    )
            }))
    }

    /// What matched, as a list of paths.
    fn file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        if self.finder.read(cx).value().trim().is_empty() {
            return self.file_tree(cx).into_any_element();
        }
        if self.file_search_scope == FileSearchScope::Project {
            return self.project_file_list(cx).into_any_element();
        }
        v_flex()
            .id("file-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py_1()
            .children(self.files.iter().map(|file| {
                let path = file.path.clone();
                h_flex()
                    .id(SharedString::from(format!("file-entry:{}", file.path)))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.open_from_list(path.clone(), event, cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(tokens.colors().text_secondary)
                            // A path's meaning is in its tail.
                            .truncate()
                            .child(file.path.clone()),
                    )
            }))
            .children((!self.matches.is_empty()).then(|| {
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .mt_2()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.files.matches").to_string())
            }))
            .children(self.matches.iter().map(|hit| {
                let path = hit.path.clone();
                v_flex()
                    .id(SharedString::from(format!(
                        "match:{}:{}",
                        hit.path, hit.line
                    )))
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_0p5()
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.open_from_list(path.clone(), event, cx)
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(format!("{}:{}", hit.path, hit.line)),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(tokens.colors().text_secondary)
                            .truncate()
                            .child(hit.text.clone()),
                    )
            }))
            .children((self.files.is_empty() && self.matches.is_empty()).then(|| {
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.files.empty").to_string())
            }))
            .into_any_element()
    }

    fn project_file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        v_flex()
            .id("project-file-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py_1()
            .children(self.project_files.iter().map(|hit| {
                let workspace = hit.workspace.clone();
                let path = hit.path.clone();
                h_flex()
                    .id(SharedString::from(format!(
                        "project-file:{}:{}",
                        hit.workspace, hit.path
                    )))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SurfaceEvent::OpenWorkspaceFile {
                            workspace: workspace.clone(),
                            path: path.clone(),
                        })
                    }))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(tokens.colors().text_secondary)
                            .truncate()
                            .child(format!("{} · {}", hit.workspace, hit.path)),
                    )
            }))
            .children((!self.project_matches.is_empty()).then(|| {
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .mt_2()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.files.matches").to_string())
            }))
            .children(self.project_matches.iter().map(|hit| {
                let workspace = hit.workspace.clone();
                let path = hit.path.clone();
                v_flex()
                    .id(SharedString::from(format!(
                        "project-match:{}:{}:{}",
                        hit.workspace, hit.path, hit.line
                    )))
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_0p5()
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SurfaceEvent::OpenWorkspaceFile {
                            workspace: workspace.clone(),
                            path: path.clone(),
                        })
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(format!("{} · {}:{}", hit.workspace, hit.path, hit.line)),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(tokens.colors().text_secondary)
                            .truncate()
                            .child(hit.text.clone()),
                    )
            }))
            .children(
                (self.project_files.is_empty() && self.project_matches.is_empty()).then(|| {
                    div()
                        .w_full()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.files.empty").to_string())
                }),
            )
    }

    /// The empty-query explorer, folded from the daemon's bounded catalogue.
    fn file_tree(&self, _: &mut Context<Self>) -> impl IntoElement + use<> {
        self.file_explorer.clone()
    }

    /// One file, as it is on disk.
    fn file_view(
        &self,
        buffer: &FileBuffer,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let file = &buffer.file;
        let editor_text = buffer
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).value().to_string());
        let state = editor_text
            .as_deref()
            .map(|text| save_state(file, text))
            .unwrap_or(SaveState::ReadOnly);
        let preview = editor_text
            .as_deref()
            .and_then(|text| markdown_preview(file, text, buffer.previewing));
        let save = (state == SaveState::Dirty).then(|| {
            let workspace = buffer.workspace.clone();
            let path = file.path.clone();
            let expected_revision = file.revision.clone();
            let text = editor_text.clone().unwrap_or_default();
            cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(SurfaceEvent::SaveFile {
                    workspace: workspace.clone(),
                    path: path.clone(),
                    text: text.clone(),
                    expected_revision: expected_revision.clone(),
                });
            })
        });
        let external_editor = self.local_paths.then(|| {
            let workspace = buffer.workspace.clone();
            let path = file.path.clone();
            let editor = buffer.editor.clone();
            cx.listener(move |_, _: &ClickEvent, _, cx| {
                let line = editor.as_ref().map(|editor| {
                    let editor = editor.read(cx);
                    ginka_ui::editor::cursor_line(&editor.value(), editor.selected_range().start)
                });
                cx.emit(SurfaceEvent::OpenExternalEditor {
                    workspace: workspace.clone(),
                    path: path.clone(),
                    line,
                });
            })
        });
        let selection = buffer.editor.as_ref().map(|editor| {
            let disabled = {
                let editor = editor.read(cx);
                saved_selection_reference(file, &editor.value(), editor.selected_range()).is_none()
            };
            let editor = editor.clone();
            let file = file.clone();
            (
                disabled,
                cx.listener(move |_, _: &ClickEvent, _, cx| {
                    let reference = {
                        let editor = editor.read(cx);
                        saved_selection_reference(&file, &editor.value(), editor.selected_range())
                    };
                    if let Some(reference) = reference {
                        cx.emit(SurfaceEvent::AddFileReference(reference));
                    }
                }),
            )
        });
        let terminal_selection = buffer.editor.as_ref().map(|editor| {
            let disabled = {
                let editor = editor.read(cx);
                selected_text(&editor.value(), editor.selected_range()).is_none()
            };
            let editor = editor.clone();
            (
                disabled,
                cx.listener(move |_, _: &ClickEvent, _, cx| {
                    let selection = {
                        let editor = editor.read(cx);
                        selected_text(&editor.value(), editor.selected_range())
                    };
                    if let Some(selection) = selection {
                        cx.emit(SurfaceEvent::WriteTerminalSelection(selection));
                    }
                }),
            )
        });
        let find = buffer.editor.as_ref().map(|editor| {
            let editor = editor.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                editor.focus_handle(cx).focus(window, cx);
                window.dispatch_action(Box::new(Search), cx);
            }
        });
        let replace = buffer.editor.as_ref().map(|editor| {
            let editor = editor.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                editor.focus_handle(cx).focus(window, cx);
                window.dispatch_action(Box::new(Replace), cx);
            }
        });
        let preview_toggle = (preview_kind(file) == Some(PreviewKind::Markdown)).then(|| {
            if buffer.previewing {
                rust_i18n::t!("surface.files.edit").to_string()
            } else {
                rust_i18n::t!("surface.files.preview").to_string()
            }
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(
                        div()
                            .id("close-file")
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(tokens.radius.row))
                            .text_xs()
                            .text_color(tokens.colors().text_muted)
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.browse_files(cx);
                            }))
                            .child(rust_i18n::t!("surface.files.back").to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(tokens.colors().text_secondary)
                            .truncate()
                            .child(file.path.clone()),
                    )
                    .child(
                        Button::new("save-file")
                            .label(rust_i18n::t!("surface.files.save").to_string())
                            .ghost()
                            .compact()
                            .small()
                            .disabled(state != SaveState::Dirty)
                            .when_some(save, |this, save| this.on_click(save)),
                    )
                    .children(external_editor.map(|open| {
                        Button::new("open-external-editor")
                            .label(rust_i18n::t!("surface.files.open_external").to_string())
                            .ghost()
                            .compact()
                            .small()
                            .disabled(state == SaveState::Dirty)
                            .on_click(open)
                    }))
                    .children(preview_toggle.map(|label| {
                        let path = file.path.clone();
                        Button::new("toggle-file-preview")
                            .label(label)
                            .ghost()
                            .compact()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_file_preview(&path, cx)
                            }))
                    }))
                    .children((!buffer.previewing).then_some(find).flatten().map(|find| {
                        Button::new("find-in-file")
                            .label(rust_i18n::t!("surface.files.find").to_string())
                            .ghost()
                            .compact()
                            .small()
                            .on_click(find)
                    }))
                    .children(
                        (!buffer.previewing)
                            .then_some(replace)
                            .flatten()
                            .map(|replace| {
                                Button::new("replace-in-file")
                                    .label(rust_i18n::t!("surface.files.replace").to_string())
                                    .ghost()
                                    .compact()
                                    .small()
                                    .on_click(replace)
                            }),
                    )
                    .children((!buffer.previewing).then_some(selection).flatten().map(
                        |(disabled, selection)| {
                            Button::new("add-file-selection")
                                .label(rust_i18n::t!("surface.files.add_selection").to_string())
                                .ghost()
                                .compact()
                                .small()
                                .disabled(disabled)
                                .on_click(selection)
                        },
                    ))
                    .children(
                        (!buffer.previewing)
                            .then_some(terminal_selection)
                            .flatten()
                            .map(|(disabled, selection)| {
                                let label =
                                    rust_i18n::t!("surface.files.send_selection_to_terminal")
                                        .to_string();
                                Button::new("send-file-selection-to-terminal")
                                    .ghost()
                                    .compact()
                                    .small()
                                    .disabled(disabled)
                                    .tooltip(label.clone())
                                    .accessibility_label(label)
                                    .child(Icon::new(IconName::SquareTerminal).size_3())
                                    .on_click(selection)
                            }),
                    ),
            )
            .children(buffer.complaint.as_ref().map(|complaint| {
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(tokens.colors().status_error)
                    .child(complaint.clone())
            }))
            .child(if let Some(markdown) = preview {
                div()
                    .id("file-preview")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("file-preview:{}", file.path)),
                            markdown,
                        )
                        .markdown_mdx(),
                    )
                    .into_any_element()
            } else if let Some(image) = &buffer.image_view {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(image.clone())
                    .into_any_element()
            } else {
                match &buffer.editor {
                    Some(editor) => div()
                        .id("file-content")
                        .flex_1()
                        .min_h_0()
                        .child(
                            Editor::new(editor)
                                .aria_label(
                                    rust_i18n::t!("surface.files.editor", path = file.path.clone())
                                        .to_string(),
                                )
                                .font_family(mono)
                                .text_size(px(12.))
                                .size_full(),
                        )
                        .into_any_element(),
                    None => v_flex()
                        .id("file-content")
                        .flex_1()
                        .min_h_0()
                        .overflow_scroll()
                        .p_3()
                        .font_family(mono)
                        .text_xs()
                        .text_color(tokens.colors().text_primary)
                        .children(file.binary.then(|| {
                            div()
                                .text_color(tokens.colors().text_muted)
                                .child(rust_i18n::t!("surface.files.binary").to_string())
                        }))
                        .children(
                            (!file.binary)
                                .then(|| div().whitespace_normal().child(file.text.clone())),
                        )
                        .children(file.truncated.then(|| {
                            div()
                                .pt_2()
                                .text_color(tokens.colors().text_muted)
                                .child(rust_i18n::t!("surface.files.truncated").to_string())
                        }))
                        .into_any_element(),
                }
            })
    }
}

/// One row of the split view: the old line on the left, the new on the
/// right, either side possibly empty. Clicking a side comments on that line,
/// as clicking a unified line does.
fn split_row(
    path: &str,
    row: ginka_ui::split_diff::SplitRow<'_>,
    tokens: &Tokens,
    mono: SharedString,
    cx: &mut Context<SurfacePanel>,
) -> AnyElement {
    let side = |line: Option<&ginka_protocol::model::DiffLine>,
                old: bool,
                cx: &mut Context<SurfacePanel>| {
        let Some(line) = line else {
            return div()
                .flex_1()
                .min_w_0()
                .bg(tokens.colors().bg_surface.opacity(0.5))
                .into_any_element();
        };
        let number = if old { line.old_line } else { line.new_line };
        // A context line is anchored to its new number either side, as the
        // unified view anchors it.
        let anchor = match line.kind {
            LineKind::Removed => line.old_line,
            _ => line.new_line,
        };
        let path = path.to_string();
        h_flex()
            .id(SharedString::from(format!(
                "split:{path}:{old}:{:?}:{number:?}",
                line.kind
            )))
            .flex_1()
            .min_w_0()
            .gap_2()
            .overflow_hidden()
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .when(line.kind == LineKind::Added, |this| {
                this.bg(tokens.colors().status_done.opacity(0.10))
            })
            .when(line.kind == LineKind::Removed, |this| {
                this.bg(tokens.colors().status_error.opacity(0.10))
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_line(path.clone(), anchor, event.modifiers().shift, window, cx)
            }))
            .child(
                div()
                    .w(px(30.))
                    .flex_shrink_0()
                    .text_color(tokens.colors().text_muted.opacity(0.7))
                    .child(number.map(|at| at.to_string()).unwrap_or_default()),
            )
            .child(diff_text(line, tokens))
            .into_any_element()
    };
    h_flex()
        .w_full()
        .px_3()
        .gap_1()
        .font_family(mono)
        .text_xs()
        .line_height(px(17.))
        .child(side(row.left, true, cx))
        .child(div().w(px(1.)).h_full().bg(tokens.colors().border_subtle))
        .child(side(row.right, false, cx))
        .into_any_element()
}

/// How a change source keys the image-diff cache: its debug form, which
/// names the kind and the commit, checkpoint or base it is measured from.
fn image_source_key(source: &ginka_protocol::model::ChangeSource) -> String {
    format!("{source:?}")
}

/// One line of a diff, with the parts that actually changed marked.
///
/// The marks are why the line is split at all: a one-word edit reads as a
/// whole line replaced, and the word is what the reader is looking for.
fn diff_text(line: &ginka_protocol::model::DiffLine, tokens: &Tokens) -> impl IntoElement + use<> {
    let colour = match line.kind {
        LineKind::Added => tokens.colors().text_primary,
        LineKind::Removed => tokens.colors().text_secondary,
        LineKind::Context => tokens.colors().text_muted,
    };
    let mark = match line.kind {
        LineKind::Added => tokens.colors().status_done.opacity(0.22),
        LineKind::Removed => tokens.colors().status_error.opacity(0.22),
        LineKind::Context => gpui::transparent_black(),
    };
    let sign = match line.kind {
        LineKind::Added => '+',
        LineKind::Removed => '-',
        LineKind::Context => ' ',
    };

    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut at = 0usize;
    for span in &line.words {
        let (start, end) = (span.start as usize, span.end as usize);
        if start > at && start <= line.text.len() {
            parts.push((line.text[at..start].to_string(), false));
        }
        if end <= line.text.len() {
            parts.push((line.text[start..end].to_string(), true));
            at = end;
        }
    }
    parts.push((line.text[at.min(line.text.len())..].to_string(), false));

    h_flex()
        .flex_1()
        .flex_wrap()
        .text_color(colour)
        .child(div().child(sign.to_string()))
        .children(parts.into_iter().filter(|(text, _)| !text.is_empty()).map(
            move |(text, changed)| {
                div()
                    .when(changed, |this| this.bg(mark).rounded(px(2.)))
                    .child(text)
            },
        ))
}

impl Render for SurfacePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A generated message lands here rather than where the event arrived,
        // because writing into the box needs the window.
        if let Some(text) = self.generated.take()
            && let Some(state) = self.message.as_ref()
        {
            state.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        if self.dock_stale {
            self.dock_stale = false;
            self.rebuild_dock(window, cx);
        }
        let tokens = Tokens::global(cx);
        let border = tokens.colors().border_subtle;
        let surface_bg = tokens.colors().bg_window;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(browser) = self
            .browser_workspace
            .as_ref()
            .and_then(|workspace| self.browser_tabs.get(workspace))
            .and_then(|browser| browser.as_ref().ok())
        {
            let visible = self.browser_on_screen();
            browser.update(cx, |browser, cx| browser.set_visible(visible, cx));
        }

        v_flex()
            .size_full()
            // The same second coat as the conversation: a diff is read, too.
            .bg(surface_bg)
            .border_l_1()
            .border_color(border)
            .child(self.toolbar(cx))
            .child(if self.choosing || self.dock.is_empty() {
                self.empty_state(cx).into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(self.dock_area.clone())
                    .into_any_element()
            })
    }
}

/// Every surface under a node, in reading order.
fn collect_surfaces(node: &DockNode, into: &mut Vec<Surface>) {
    match node {
        DockNode::Split { children, .. } => {
            for child in children {
                collect_surfaces(child, into);
            }
        }
        DockNode::Tabs { surfaces, .. } => into.extend(surfaces.iter().copied()),
    }
}

/// One surface as a tab of the right panel's dock area.
///
/// It owns nothing of the surface: the panel keeps every surface's state, so
/// a tab dragged elsewhere, or closed and opened again, shows the same diff,
/// search and page it did before.
pub struct SurfaceTab {
    surface: Surface,
    panel: WeakEntity<SurfacePanel>,
    focus: FocusHandle,
}

impl SurfaceTab {
    fn new(surface: Surface, panel: &Entity<SurfacePanel>, cx: &mut Context<Self>) -> Self {
        // The panel's state is what the tab draws, so a change to it is a
        // change to the tab.
        cx.observe(panel, |_, _, cx| cx.notify()).detach();
        Self {
            surface,
            panel: panel.downgrade(),
            focus: cx.focus_handle(),
        }
    }
}

impl EventEmitter<PanelEvent> for SurfaceTab {}

impl Focusable for SurfaceTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for SurfaceTab {
    fn panel_name(&self) -> &'static str {
        self.surface.panel_name()
    }
}

impl Panel for SurfaceTab {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // In front when the panel's own arrangement says so, which is what
        // the dock was built from and read back into.
        let active = self
            .panel
            .upgrade()
            .is_some_and(|panel| panel.read(cx).shows(self.surface));
        // What the tab holds; the tab itself — the same one as everywhere
        // else — is drawn around it by the dock's tab bar
        // (`crate::surface_dock`), which also owns closing it.
        h_flex()
            .gap(px(6.))
            .items_center()
            .child(ginka_ui::chrome::tab_icon(self.surface.icon(), active, cx))
            .child(ginka_ui::chrome::tab_label(self.surface.title()))
    }

    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.surface.title().into())
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for SurfaceTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surface = self.surface;
        let body = self
            .panel
            .update(cx, |panel, cx| panel.surface_body(surface, cx))
            .unwrap_or_else(|_| div().into_any_element());
        // Bounded by the tab rather than by what is in it: a long line in one
        // surface would otherwise widen the whole split, and every surface
        // beside it would run out under the window's edge.
        // Faded in as the tab comes to the front, the way panels arrive.
        ginka_ui::motion::fade_in(
            SharedString::from(format!("surface-body:{}", surface.key())),
            v_flex()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .track_focus(&self.focus)
                .child(body),
        )
    }
}

/// How tall one history row is: the lanes are drawn to it.
const GRAPH_ROW: f32 = 30.;
/// How far apart the lanes are.
const GRAPH_LANE: f32 = 12.;

/// Draw one row of the git graph: the lanes passing through, the lanes
/// ending at the commit, the ones leaving it for its parents, and its dot —
/// ringed for a merge.
fn paint_graph_row(
    row: &ginka_ui::graph::GraphRow,
    bounds: Bounds<Pixels>,
    palette: &[Hsla; 5],
    window: &mut Window,
) {
    let x = |lane: usize| bounds.origin.x + px(10. + lane.min(5) as f32 * GRAPH_LANE);
    let top = bounds.origin.y;
    let middle = bounds.origin.y + bounds.size.height / 2.;
    let bottom = bounds.origin.y + bounds.size.height;
    let color = |lane: usize| palette[lane % palette.len()];
    let stroke = |from: Point<Pixels>, to: Point<Pixels>, color: Hsla, window: &mut Window| {
        let mut path = PathBuilder::stroke(px(1.5));
        path.move_to(from);
        if from.x == to.x {
            path.line_to(to);
        } else {
            // A curve out of the row's middle, so a lane bends into its
            // neighbour rather than kinking.
            path.curve_to(to, point(to.x, from.y));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    for lane in &row.through {
        stroke(
            point(x(*lane), top),
            point(x(*lane), bottom),
            color(*lane),
            window,
        );
    }
    for lane in &row.into {
        stroke(
            point(x(*lane), top),
            point(x(row.lane), middle),
            color(*lane),
            window,
        );
    }
    for lane in &row.out {
        stroke(
            point(x(row.lane), middle),
            point(x(*lane), bottom),
            color(*lane),
            window,
        );
    }
    let merge = row.out.len() > 1;
    let radius = px(if merge { 4. } else { 3.5 });
    window.paint_quad(
        fill(
            Bounds::centered_at(point(x(row.lane), middle), size(radius * 2., radius * 2.)),
            color(row.lane),
        )
        .corner_radii(radius),
    );
}
