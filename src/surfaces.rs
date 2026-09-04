//! The right panel: `docs/ui.md` §3.4.
//!
//! A surface is whatever the user wants beside the transcript — a terminal, git,
//! files, an editor, later a browser. Git is real: it draws what the agent
//! changed. The rest are placeholders until M3 and M4. M4 turns this into a
//! `DockArea` so surfaces can be dragged, split and persisted per workspace.

use ginka_protocol::model::{ChangeKind, Changes, ContentMatch, FileContent, FileEntry, LineKind};
use ginka_ui::Tokens;
use ginka_ui::surface::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Icon, IconName, h_flex, v_flex};

pub struct SurfacePanel {
    open: Option<Surface>,
    /// What the workspace on screen has changed, as the shell last read it.
    changes: Option<Changes>,
    /// The file whose diff is expanded. A review starts as a list of files:
    /// twelve diffs at once is not a review, it is a wall.
    expanded: Option<String>,
    /// The comments waiting to go back to the agent.
    comments: Vec<ginka_protocol::model::ReviewComment>,
    /// The line a comment is being written on, and the box it is written in.
    ///
    /// One at a time: a review is read line by line, and two open boxes is a
    /// form, not a margin note.
    commenting: Option<(String, Option<u32>, Entity<TextareaState>)>,
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
    /// The commit message being written, if the box is open.
    message: Option<Entity<TextareaState>>,
    /// Why the last commit did not happen.
    complaint: Option<SharedString>,
    /// What is typed into the file finder.
    finder: Entity<InputState>,
    /// The paths that match it, best first.
    files: Vec<FileEntry>,
    /// The lines that contain what was typed, if any do.
    matches: Vec<ContentMatch>,
    /// The file being read, if one was opened.
    showing: Option<FileContent>,
}

/// Emitted when the panel wants the shell to do something only it can.
pub enum SurfaceEvent {
    /// Commit the workspace's work with this message.
    ///
    /// `only_staged` when the reader has staged something: having said which
    /// files belong in the commit, they do not expect the rest to come along.
    Commit { message: String, only_staged: bool },
    /// Leave a comment on a file, and a line of it.
    Comment {
        path: String,
        line: Option<u32>,
        text: String,
    },
    /// Send every waiting comment back to the agent.
    SendReview,
    /// Look for files whose path matches this.
    FindFiles(String),
    /// Read a file and show it.
    OpenFile(String),
    /// Put a file into the next commit, or take it back out.
    Stage { path: String, staged: bool },
    /// Throw away a file's uncommitted work.
    Revert { path: String },
}

impl EventEmitter<SurfaceEvent> for SurfacePanel {}

impl SurfacePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        Self {
            finder,
            files: Vec::new(),
            matches: Vec::new(),
            showing: None,
            open: None,
            changes: None,
            expanded: None,
            comments: Vec::new(),
            commenting: None,
            staged: Vec::new(),
            reverting: None,
            message: None,
            complaint: None,
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

    /// Hand the panel the lines that matched what was typed.
    pub fn set_matches(&mut self, matches: Vec<ContentMatch>, cx: &mut Context<Self>) {
        if self.matches != matches {
            self.matches = matches;
            cx.notify();
        }
    }

    /// Show a file that has been read.
    pub fn set_file(&mut self, file: Option<FileContent>, cx: &mut Context<Self>) {
        self.showing = file;
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
        !self.staged.is_empty()
    }

    /// Say why a commit did not happen, or clear it once one did.
    pub fn set_commit_result(&mut self, complaint: Option<String>, cx: &mut Context<Self>) {
        self.complaint = complaint.map(SharedString::from);
        if self.complaint.is_none() {
            // It went in; the message belongs to the commit now.
            self.message = None;
        }
        cx.notify();
    }

    /// Show a surface, which is what the palette does when it is asked for
    /// one.
    pub fn show(&mut self, surface: Surface, cx: &mut Context<Self>) {
        self.open = Some(surface);
        // A finder that opens empty is one the user has to type into before it
        // says anything; the start of the list is what a picker shows before
        // anything is typed.
        if surface == Surface::Files && self.files.is_empty() {
            cx.emit(SurfaceEvent::FindFiles(String::new()));
        }
        cx.notify();
    }

    /// Which surface is showing, so the shell knows what to keep fetching.
    pub fn open_surface(&self) -> Option<Surface> {
        self.open
    }

    /// Hand the panel what changed. Called from the shell's refresh.
    pub fn set_changes(&mut self, changes: Option<Changes>, cx: &mut Context<Self>) {
        if self.changes != changes {
            // A file that is no longer in the list cannot stay expanded.
            if let Some(path) = &self.expanded
                && !changes
                    .as_ref()
                    .is_some_and(|changes| changes.files.iter().any(|file| &file.path == path))
            {
                self.expanded = None;
            }
            self.changes = changes;
            cx.notify();
        }
    }

    fn toolbar(&self, cx: &App) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .justify_between()
            .items_center()
            .child(
                Icon::new(IconName::Plus)
                    .size_4()
                    .text_color(tokens.colors().text_secondary),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Maximize)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    )
                    .child(
                        Icon::new(IconName::PanelRight)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    ),
            )
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
                    .child(surface.label()),
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

        if changes.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.clean").to_string()),
                )
                .into_any_element();
        }

        let (added, removed) = changes.totals();
        v_flex()
            .id("git-surface")
            .flex_1()
            .overflow_y_scroll()
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
                            .child(
                                rust_i18n::t!("surface.git.summary", files = changes.files.len())
                                    .to_string(),
                            ),
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
            .children(
                changes
                    .files
                    .iter()
                    .map(|file| self.file_row(file, cx).into_any_element()),
            )
            .children(self.review_bar(cx))
            .child(self.commit_box(cx))
            .into_any_element()
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
        self.commenting = Some((path, line, state));
        cx.notify();
    }

    /// Hand a finished comment to the shell, which is the one holding the
    /// daemon.
    fn finish_comment(&mut self, text: String, cx: &mut Context<Self>) {
        let Some((path, line, _)) = self.commenting.take() else {
            return;
        };
        cx.notify();
        if text.is_empty() {
            return;
        }
        cx.emit(SurfaceEvent::Comment { path, line, text });
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
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .ml_8()
                    .border_l_2()
                    .border_color(tokens.colors().accent)
                    .bg(tokens.colors().row_hover())
                    .text_xs()
                    .text_color(tokens.colors().text_primary)
                    .child(comment.text.clone())
                    .into_any_element()
            })
            .collect();

        if let Some((open_path, open_line, state)) = &self.commenting
            && open_path == path
            && *open_line == line
        {
            drawn.push(
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .ml_8()
                    .border_l_2()
                    .border_color(tokens.colors().accent)
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
        let open = self.message.clone();

        v_flex()
            .w_full()
            .p_2()
            .gap_1p5()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
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
            .child(match open {
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
                                div()
                                    .id("commit")
                                    .px_2p5()
                                    .py_1()
                                    .rounded(px(tokens.radius.row))
                                    .bg(tokens.colors().accent.opacity(0.9))
                                    .text_xs()
                                    .text_color(tokens.colors().bg_window)
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().accent))
                                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx)))
                                    .child(rust_i18n::t!("surface.git.commit").to_string()),
                            )
                            .child(
                                div()
                                    .id("cancel-commit")
                                    .px_2p5()
                                    .py_1()
                                    .rounded(px(tokens.radius.row))
                                    .text_xs()
                                    .text_color(tokens.colors().text_muted)
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.message = None;
                                        this.complaint = None;
                                        cx.notify();
                                    }))
                                    .child(rust_i18n::t!("surface.git.cancel").to_string()),
                            ),
                    )
                    .into_any_element(),
                None => div()
                    .id("write-commit")
                    .w_full()
                    .px_2p5()
                    .py_1p5()
                    .rounded(px(tokens.radius.row))
                    .text_xs()
                    .text_color(tokens.colors().text_secondary)
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(|this, _, window, cx| this.write_commit(window, cx)))
                    .child(rust_i18n::t!("surface.git.write_commit").to_string())
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
    fn commit(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.message.as_ref() else {
            return;
        };
        let message = state.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.complaint = Some(
                rust_i18n::t!("surface.git.needs_message")
                    .to_string()
                    .into(),
            );
            cx.notify();
            return;
        }
        cx.emit(SurfaceEvent::Commit {
            message,
            only_staged: self.only_staged(),
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
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let staged = self.staged.iter().any(|path| path == &file.path);
        let asking = self.reverting.as_deref() == Some(file.path.as_str());
        let path = file.path.clone();
        let to_stage = path.clone();
        let to_revert = path.clone();
        let to_arm = path.clone();

        let mut actions: Vec<AnyElement> = vec![
            div()
                .id(SharedString::from(format!("stage:{path}")))
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

        if asking {
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
        actions
    }

    /// One file, and its diff when it is the one being read.
    fn file_row(
        &self,
        file: &ginka_protocol::model::FileChange,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let expanded = self.expanded.as_deref() == Some(file.path.as_str());
        let path = file.path.clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();

        v_flex()
            .w_full()
            .child(
                h_flex()
                    .id(SharedString::from(format!("file:{}", file.path)))
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
                    .children(self.file_actions(file, cx)),
            )
            .when(expanded && file.binary, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.git.binary").to_string()),
                )
            })
            .when(expanded && !file.binary, |this| {
                this.children(file.hunks.iter().map(|hunk| {
                    v_flex()
                        .w_full()
                        .child(
                            div()
                                .w_full()
                                .px_3()
                                .py_0p5()
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(tokens.colors().text_muted)
                                .bg(tokens.colors().bg_surface)
                                .child(hunk.header.clone()),
                        )
                        .children(hunk.lines.iter().map(|line| {
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
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.comment_on(path.clone(), anchor, window, cx)
                                }))
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
                                .child(diff_text(line, &tokens))
                                .into_any_element()
                        }))
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
                    .child(surface.label()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(surface.availability()),
            )
    }
}

impl SurfacePanel {
    /// The files surface: a finder over the worktree, and what it opens.
    ///
    /// Read-only, and honestly so — the editor is M4. What it is for now is
    /// the question a diff raises and cannot answer: what does the rest of
    /// this file look like.
    fn files(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .w_full()
                    .p_2()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(Input::new(&self.finder)),
            )
            .child(match &self.showing {
                Some(file) => self.file_view(file, mono, cx).into_any_element(),
                None => self.file_list(cx).into_any_element(),
            })
    }

    /// What matched, as a list of paths.
    fn file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
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
                    .on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(SurfaceEvent::OpenFile(path.clone()))
                        }),
                    )
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
                    .on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(SurfaceEvent::OpenFile(path.clone()))
                        }),
                    )
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
    }

    /// One file, as it is on disk.
    fn file_view(
        &self,
        file: &FileContent,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
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
                                this.showing = None;
                                cx.notify();
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
                    ),
            )
            .child(
                v_flex()
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
                        (!file.binary).then(|| div().whitespace_normal().child(file.text.clone())),
                    )
                    .children(file.truncated.then(|| {
                        div()
                            .pt_2()
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("surface.files.truncated").to_string())
                    })),
            )
    }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx);
        let border = tokens.colors().border_subtle;
        let open = self.open;

        v_flex()
            .size_full()
            .border_l_1()
            .border_color(border)
            .child(self.toolbar(cx))
            .child(match open {
                None => self.empty_state(cx).into_any_element(),
                Some(Surface::Git) => self.git(cx).into_any_element(),
                Some(Surface::Files) => self.files(cx).into_any_element(),
                Some(surface) => self.placeholder(surface, cx).into_any_element(),
            })
    }
}
