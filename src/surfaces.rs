//! The right panel: `docs/ui.md` §3.4.
//!
//! A surface is whatever the user wants beside the transcript — a terminal, git,
//! files, an editor, later a browser. Git is real: it draws what the agent
//! changed. The rest are placeholders until M3 and M4. M4 turns this into a
//! `DockArea` so surfaces can be dragged, split and persisted per workspace.

use ginka_protocol::model::{ChangeKind, Changes, LineKind};
use ginka_ui::Tokens;
use ginka_ui::surface::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{Icon, IconName, h_flex, v_flex};

pub struct SurfacePanel {
    open: Option<Surface>,
    /// What the workspace on screen has changed, as the shell last read it.
    changes: Option<Changes>,
    /// The file whose diff is expanded. A review starts as a list of files:
    /// twelve diffs at once is not a review, it is a wall.
    expanded: Option<String>,
}

impl SurfacePanel {
    pub fn new() -> Self {
        Self {
            open: None,
            changes: None,
            expanded: None,
        }
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open = Some(surface);
                cx.notify();
            }))
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
            .into_any_element()
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
                    ),
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
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(match line.kind {
                                            LineKind::Added => tokens.colors().text_primary,
                                            LineKind::Removed => tokens.colors().text_secondary,
                                            LineKind::Context => tokens.colors().text_muted,
                                        })
                                        .child(format!(
                                            "{}{}",
                                            match line.kind {
                                                LineKind::Added => '+',
                                                LineKind::Removed => '-',
                                                LineKind::Context => ' ',
                                            },
                                            line.text
                                        )),
                                )
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
                Some(surface) => self.placeholder(surface, cx).into_any_element(),
            })
    }
}
