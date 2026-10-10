//! Virtualized, keyboard-accessible workspace explorer; durable file reads stay in the daemon.

use crate::Tokens;
use crate::file_tree::{FileTree, TREE_FILE_LIMIT, TreeNavigation, TreeRow, TreeRowKind};
use ginka_protocol::model::FileEntry;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::ListItem;
use gpui_component::{Icon, IconName, h_flex, v_flex};
use std::time::Instant;

const ROW_HEIGHT: f32 = 28.;

/// A file activated through a click or the keyboard.
pub struct ExplorerOpen {
    /// Workspace-relative path passed to the shared file-read request.
    pub path: String,
    /// A single click uses the replaceable preview tab; other activations pin it.
    pub preview: bool,
}

/// Bounded catalogue with path-based selection and virtualized visible rows.
pub struct FileExplorer {
    files: Vec<FileEntry>,
    tree: FileTree,
    rows: Vec<TreeRow>,
    truncated: bool,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<ExplorerOpen> for FileExplorer {}

impl FileExplorer {
    /// Create an empty explorer with its own keyboard focus and scroll position.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            files: Vec::new(),
            tree: FileTree::default(),
            rows: Vec::new(),
            truncated: false,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Replace the daemon catalogue, retaining expansion and any surviving selection.
    pub fn set_files(&mut self, files: Vec<FileEntry>, truncated: bool, cx: &mut Context<Self>) {
        if self.files == files && self.truncated == truncated {
            return;
        }
        let (files, clipped) = crate::file_tree::bounded_catalogue(files, TREE_FILE_LIMIT);
        self.files = files;
        self.truncated = truncated || clipped;
        self.refresh(cx);
    }

    /// Forget workspace-local navigation when another workspace is selected.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.files.clear();
        self.tree = FileTree::default();
        self.rows.clear();
        self.truncated = false;
        self.scroll = UniformListScrollHandle::new();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.tree.reconcile(&self.files);
        self.rows = self.tree.rows(&self.files);
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| Some(row.path.as_str()) == self.tree.selected())
        {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let key = &event.keystroke;
        if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        }
        let action = match key.key.as_str() {
            "up" => Some(TreeNavigation::Previous),
            "down" => Some(TreeNavigation::Next),
            "home" => Some(TreeNavigation::First),
            "end" => Some(TreeNavigation::Last),
            "pageup" => Some(TreeNavigation::PageUp),
            "pagedown" => Some(TreeNavigation::PageDown),
            "left" => Some(TreeNavigation::Left),
            "right" => Some(TreeNavigation::Right),
            "enter" | "space" => Some(TreeNavigation::Activate),
            _ => None,
        };
        let handled = if let Some(action) = action {
            let height = f32::from(self.scroll.0.borrow().base_handle.bounds().size.height);
            let page = (height / ROW_HEIGHT).floor().max(1.) as usize;
            if let Some(path) = self.tree.navigate(&self.files, action, page) {
                cx.emit(ExplorerOpen {
                    path,
                    preview: false,
                });
            }
            true
        } else {
            key.key_char
                .as_deref()
                .is_some_and(|text| self.tree.type_prefix(&self.files, text, Instant::now()))
        };
        if handled {
            self.refresh(cx);
            cx.stop_propagation();
        }
    }
}

impl Focusable for FileExplorer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FileExplorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let focused = self.focus.is_focused(window);
        v_flex()
            .id("workspace-file-tree")
            .flex_1()
            .min_h_0()
            .track_focus(&self.focus)
            .tab_stop(true)
            .border_1()
            .border_color(if focused {
                tokens.colors().accent
            } else {
                tokens.colors().border_subtle
            })
            .capture_key_down(cx.listener(Self::key_down))
            .children(self.truncated.then(|| {
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(tokens.colors().status_error)
                    .child(rust_i18n::t!("surface.files.tree.truncated", limit = TREE_FILE_LIMIT).to_string())
            }))
            .when(self.rows.is_empty(), |view| {
                view.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(tokens.colors().text_muted)
                        .child(rust_i18n::t!("surface.files.tree.empty").to_string()),
                )
            })
            .when(!self.rows.is_empty(), |view| {
                view.child(
                    uniform_list(
                        "explorer-rows",
                        self.rows.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                            let tokens = Tokens::global(cx).clone();
                            let focused = this.focus.is_focused(window);
                            range
                                .map(|index| {
                                    let row = this.rows[index].clone();
                                    let path = row.path.clone();
                                    let directory = row.kind == TreeRowKind::Directory;
                                    let selected = this.tree.selected() == Some(row.path.as_str());
                                    let icon = match (directory, row.expanded) {
                                        (true, true) => IconName::FolderOpen,
                                        (true, false) => IconName::Folder,
                                        (false, _) => IconName::File,
                                    };
                                    ListItem::new(SharedString::from(format!("explorer:{}", row.path)))
                                        .h(px(ROW_HEIGHT))
                                        .w_full()
                                        .selected(selected)
                                        .when(selected && focused, |item| {
                                            item.border_1().border_color(tokens.colors().accent)
                                        })
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .min_w_0()
                                                .pl(px(8. + row.depth as f32 * 14.
                                                    + if directory { 0. } else { 18. }))
                                                .gap_1p5()
                                                .when(directory, |view| {
                                                    view.child(
                                                        Icon::new(if row.expanded {
                                                            IconName::ChevronDown
                                                        } else {
                                                            IconName::ChevronRight
                                                        })
                                                        .size_3()
                                                        .text_color(tokens.colors().text_muted),
                                                    )
                                                })
                                                .child(Icon::new(icon).size_4().text_color(tokens.colors().text_secondary))
                                                .child(div().flex_1().min_w_0().text_sm().truncate().child(row.name)),
                                        )
                                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                            this.focus.focus(window, cx);
                                            this.tree.select(&path);
                                            if directory {
                                                this.tree.toggle(&path);
                                            } else {
                                                cx.emit(ExplorerOpen {
                                                    path: path.clone(),
                                                    preview: matches!(event, ClickEvent::Mouse(click) if click.up.click_count < 2),
                                                });
                                            }
                                            this.refresh(cx);
                                        }))
                                        .into_any_element()
                                })
                                .collect()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0(),
                )
            })
    }
}
