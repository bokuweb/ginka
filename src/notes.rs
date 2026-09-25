//! Notes: `docs/ui.md` §3.6.
//!
//! The reader's markdown notebook, kept by the daemon (`ginka notes` and the
//! MCP tools read the same ones). A list column beside the navigation, and the
//! note being written in the centre, with a preview a click away. Saved as it
//! is typed, after a pause: a notebook with a save button is a notebook that
//! loses the last paragraph.

use crate::daemon::DaemonLink;
use ginka_protocol::ProjectName;
use ginka_protocol::model::Note;
use ginka_ui::Tokens;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::text::TextView;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, h_flex, v_flex};
use std::sync::Arc;
use std::time::Duration;

/// How long typing has to pause before the note is written.
const SAVE_AFTER: Duration = Duration::from_millis(600);

pub struct NotesView {
    link: Arc<DaemonLink>,
    /// The project whose notes are listed; every note when `None`.
    project: Option<ProjectName>,
    notes: Vec<Note>,
    /// The note in the editor. `None` with the editor filled is a new note
    /// that has not been written yet.
    editing: Option<String>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    /// Markdown rendered instead of the source.
    preview: bool,
    /// Bumped on every keystroke; a pending save only writes if it is still
    /// the latest, which is what makes the pause a pause.
    generation: u64,
    /// The delete button was pressed once and waits for the second press.
    removing: bool,
    /// Whether the fields are being filled by the view, not the reader, so
    /// their change events are not saves.
    loading: bool,
    /// A note to put into the fields at the next render, which is the first
    /// place with a window.
    pending: Option<(String, String)>,
    /// Whether the editor is open: on a note, or on a new one.
    composing: bool,
}

impl NotesView {
    pub fn new(link: Arc<DaemonLink>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("notes.title").to_string())
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("notes.body").to_string())
                .auto_grow(12, 400)
        });
        cx.subscribe(&title, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.edited(cx);
            }
        })
        .detach();
        cx.subscribe(&body, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.edited(cx);
            }
        })
        .detach();
        Self {
            link,
            project: None,
            notes: Vec::new(),
            editing: None,
            title,
            body,
            preview: false,
            generation: 0,
            removing: false,
            loading: false,
            pending: None,
            composing: false,
        }
    }

    /// List a project's notes — or all of them — and read them again.
    pub fn show_project(&mut self, project: Option<ProjectName>, cx: &mut Context<Self>) {
        self.project = project;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let link = self.link.clone();
        let project = self.project.clone();
        cx.spawn(async move |this, cx| {
            let notes = cx
                .background_spawn(async move { link.notes(project).await })
                .await;
            this.update(cx, |this, cx| {
                this.notes = notes;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open(&mut self, note: Option<&Note>, cx: &mut Context<Self>) {
        self.editing = note.map(|note| note.id.clone());
        self.pending = Some(match note {
            Some(note) => (note.title.clone(), note.body.clone()),
            None => (String::new(), String::new()),
        });
        self.preview = note.is_some_and(|note| !note.body.is_empty());
        self.removing = false;
        self.composing = true;
        cx.notify();
    }

    /// The reader typed: write it once they pause.
    fn edited(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.save(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let title = self.title.read(cx).value().to_string();
        let body = self.body.read(cx).value().to_string();
        if self.editing.is_none() && title.trim().is_empty() && body.trim().is_empty() {
            return;
        }
        let link = self.link.clone();
        let id = self.editing.clone();
        let project = self.project.clone();
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move { link.save_note(id, project, title, body).await })
                .await;
            this.update(cx, |this, cx| {
                if let Some(note) = saved {
                    // A new note is this one from now on, or the next pause
                    // would write a second copy of it.
                    if this.editing.is_none() {
                        this.editing = Some(note.id.clone());
                    }
                    this.notes.retain(|known| known.id != note.id);
                    this.notes.insert(0, note);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editing.clone() else {
            // A new note nobody wrote anything in: closing it is removing it.
            self.composing = false;
            self.pending = Some((String::new(), String::new()));
            cx.notify();
            return;
        };
        if !self.removing {
            self.removing = true;
            cx.notify();
            return;
        }
        let link = self.link.clone();
        self.notes.retain(|note| note.id != id);
        self.editing = None;
        self.pending = Some((String::new(), String::new()));
        self.removing = false;
        self.composing = false;
        cx.background_spawn(async move { link.remove_note(id).await })
            .detach();
        cx.notify();
    }

    /// The list column: a heading, the way to write a new one, the notes.
    pub fn list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = Tokens::global(cx).clone();
        let heading = self
            .project
            .as_ref()
            .map(|project| project.0.clone())
            .unwrap_or_else(|| rust_i18n::t!("notes.all").to_string());
        v_flex()
            .size_full()
            .bg(tokens.colors().bg_sidebar)
            .border_r_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                h_flex()
                    .h(ginka_ui::layout::HEADER_HEIGHT)
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::empty()
                            .path(ginka_ui::assets::icon::NOTEBOOK)
                            .size_4()
                            .text_color(tokens.colors().text_secondary),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(rust_i18n::t!("nav.notes").to_string()),
                    )
                    .child(
                        div()
                            .id("note-new")
                            .p_1()
                            .rounded(px(tokens.radius.row))
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().bg_raised))
                            .tooltip(|window, cx| {
                                Tooltip::new(rust_i18n::t!("notes.new").to_string())
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.open(None, cx)))
                            .child(
                                Icon::empty()
                                    .path(ginka_ui::assets::icon::FILE_PLUS)
                                    .size_4()
                                    .text_color(tokens.colors().text_secondary),
                            ),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .pb_2()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(heading),
            )
            .child(
                v_flex()
                    .id("notes-list")
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .children(self.notes.iter().map(|note| {
                        let chosen = self.editing.as_deref() == Some(note.id.as_str());
                        let picked = note.clone();
                        let first_line = note
                            .body
                            .lines()
                            .map(str::trim)
                            .find(|line| !line.is_empty() && !line.starts_with('#'))
                            .unwrap_or_default()
                            .to_string();
                        v_flex()
                            .id(SharedString::from(format!("note:{}", note.id)))
                            .w_full()
                            .px_3()
                            .py_2()
                            .gap_0p5()
                            .rounded(px(tokens.radius.row))
                            .cursor_pointer()
                            .when(chosen, |this| this.bg(tokens.colors().row_active()))
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.open(Some(&picked), cx)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(tokens.colors().text_primary)
                                            .truncate()
                                            .child(if note.title.is_empty() {
                                                rust_i18n::t!("notes.untitled").to_string()
                                            } else {
                                                note.title.clone()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(tokens.colors().text_muted)
                                            .child(ginka_ui::workspace::relative_age(
                                                crate::daemon::now(),
                                                note.updated_at,
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(tokens.colors().text_muted)
                                    .truncate()
                                    .child(first_line),
                            )
                    }))
                    .children(self.notes.is_empty().then(|| {
                        div()
                            .px_3()
                            .py_4()
                            .text_size(px(12.5))
                            .text_color(tokens.colors().text_muted)
                            .child(rust_i18n::t!("notes.empty").to_string())
                    })),
            )
    }

    /// The editor, in the centre column.
    pub fn editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some((title, body)) = self.pending.take() {
            self.loading = true;
            self.title
                .update(cx, |state, cx| state.set_value(title, window, cx));
            self.body
                .update(cx, |state, cx| state.set_value(body, window, cx));
            self.loading = false;
        }
        let tokens = Tokens::global(cx).clone();
        if !self.composing {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(tokens.colors().text_secondary)
                        .child(rust_i18n::t!("notes.pick").to_string()),
                )
                .child(
                    div()
                        .id("note-new-centre")
                        .px_3()
                        .py_1p5()
                        .rounded(px(tokens.radius.control()))
                        .bg(tokens.colors().row_active())
                        .text_size(px(12.5))
                        .text_color(tokens.colors().text_primary)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.open(None, cx)))
                        .child(rust_i18n::t!("notes.new").to_string()),
                )
                .into_any_element();
        }
        let preview = self.preview;
        let body_text = self.body.read(cx).value().to_string();
        let segment = |id: &'static str, label: String, on: bool| {
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded(px(5.))
                .text_size(px(11.5))
                .cursor_pointer()
                .when(on, |this| this.bg(tokens.colors().row_active()))
                .text_color(if on {
                    tokens.colors().text_primary
                } else {
                    tokens.colors().text_muted
                })
                .child(label)
        };

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(ginka_ui::layout::HEADER_HEIGHT)
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .p_0p5()
                            .gap_0p5()
                            .rounded(px(tokens.radius.row))
                            .bg(tokens.colors().bg_surface)
                            .child(
                                segment(
                                    "note-edit",
                                    rust_i18n::t!("notes.edit").to_string(),
                                    !preview,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.preview = false;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                segment(
                                    "note-preview",
                                    rust_i18n::t!("notes.preview").to_string(),
                                    preview,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.preview = true;
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .id("note-remove")
                            .px_2()
                            .py_0p5()
                            .rounded(px(tokens.radius.control()))
                            .text_size(px(11.5))
                            .cursor_pointer()
                            .when(self.removing, |this| {
                                this.bg(tokens.colors().status_error.opacity(0.22))
                            })
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .text_color(if self.removing {
                                tokens.colors().text_primary
                            } else {
                                tokens.colors().text_muted
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.remove(cx)))
                            .child(if self.removing {
                                rust_i18n::t!("notes.remove.confirm").to_string()
                            } else {
                                rust_i18n::t!("notes.remove").to_string()
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("note-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(720.))
                            .mx_auto()
                            .px_6()
                            .py_5()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(Input::new(&self.title).appearance(false)),
                            )
                            .child(if preview {
                                div()
                                    .text_size(px(14.))
                                    .line_height(px(22.4))
                                    .text_color(tokens.colors().text_primary)
                                    .child(
                                        TextView::markdown("note-markdown", body_text)
                                            .selectable(true),
                                    )
                                    .into_any_element()
                            } else {
                                div()
                                    .text_size(px(14.))
                                    .child(Textarea::new(&self.body).appearance(false))
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for NotesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = self.list(cx);
        let editor = self.editor(window, cx);
        h_flex()
            .size_full()
            .child(div().w(px(300.)).h_full().flex_shrink_0().child(list))
            .child(div().flex_1().min_w_0().h_full().child(editor))
    }
}
