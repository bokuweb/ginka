//! The files column: a repository's tree, walked or searched.
//!
//! One request fetches every path in the repository, so both what is drawn
//! here answer locally. With the search box empty the paths are a file tree
//! — folders that fold, as on GitHub — and with anything typed in it they
//! are the matches for what was typed, flat, because a match is about the
//! whole path and not about where it sits. Either way the rows are a
//! `uniform_list`: a repository can hold twenty thousand paths and a column
//! that built an element for each would not answer between keystrokes.

use crate::store::{Store, StoreEvent};
use e1_github::RepoId;
use e1_ui::tree::Tree;
use e1_ui::{Tokens, finder};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName, h_flex, v_flex};
use std::collections::HashSet;

/// How tall a path row is.
const ROW_HEIGHT: Pixels = px(28.);

/// How far each level of the tree sits in from the one over it.
const INDENT: f32 = 13.;

/// How many matches are listed. Past this the reader types another letter.
const MATCH_CAP: usize = 400;

/// Emitted when the reader picks a file.
pub enum BrowserEvent {
    /// Read this file.
    Open {
        /// Which repository.
        repo: RepoId,
        /// Which path.
        path: String,
    },
}

impl EventEmitter<BrowserEvent> for FileBrowser {}

/// The files column.
pub struct FileBrowser {
    store: Entity<Store>,
    repo: Option<RepoId>,
    query: Entity<InputState>,
    /// Every file path in the tree, in GitHub's order, for the search.
    paths: Vec<String>,
    /// The paths that match the query, as indices into `paths`.
    matches: Vec<usize>,
    /// The same paths, folded into directories.
    tree: Tree,
    /// The directories that are open, by path.
    expanded: HashSet<String>,
    /// The tree's nodes that are on screen, in order.
    rows: Vec<usize>,
    /// Whether GitHub cut the tree short.
    truncated: bool,
    selected: Option<String>,
}

impl FileBrowser {
    /// A files column over a store, showing nothing until told which
    /// repository.
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("files.search").to_string())
        });
        cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.rematch(cx);
            }
        })
        .detach();
        cx.subscribe(&store, |this, _, _: &StoreEvent, cx| this.rebuild(cx))
            .detach();
        Self {
            store,
            repo: None,
            query,
            paths: Vec::new(),
            matches: Vec::new(),
            tree: Tree::default(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            truncated: false,
            selected: None,
        }
    }

    /// Show a repository's files, fetching the tree if it never has been.
    pub fn set_repo(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.repo.as_ref() != Some(&repo) {
            // Another repository's folders say nothing about this one's.
            self.selected = None;
            self.expanded.clear();
        }
        self.repo = Some(repo.clone());
        self.store
            .update(cx, |store, cx| store.ensure_tree(repo, cx));
        self.rebuild(cx);
    }

    /// Which repository is on screen.
    pub fn repo(&self) -> Option<&RepoId> {
        self.repo.as_ref()
    }

    /// Fetch the tree again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.clone() {
            self.store.update(cx, |store, cx| store.load_tree(repo, cx));
        }
    }

    /// Whether the tree is being fetched.
    pub fn is_loading(&self, cx: &App) -> bool {
        self.repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).tree(repo))
            .is_some_and(|tree| tree.is_loading())
    }

    /// Mark a path as the one being read, unfolding the way down to it.
    ///
    /// What the column does when a file was opened from somewhere else —
    /// a launch argument, or a link — so the tree agrees with the panel.
    pub fn reveal(&mut self, path: &str, cx: &mut Context<Self>) {
        self.selected = Some(path.to_string());
        self.expanded.extend(e1_ui::tree::ancestors(path));
        self.rebuild_rows();
        cx.notify();
    }

    /// Rebuild the paths, the tree and the matches from the store's tree.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let tree = self
            .repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).tree(repo))
            .and_then(|fetch| fetch.value())
            .cloned();
        match tree {
            Some(tree) => {
                self.truncated = tree.truncated;
                self.paths = tree.files().map(|entry| entry.path.clone()).collect();
            }
            None => {
                self.truncated = false;
                self.paths.clear();
            }
        }
        self.tree = Tree::build(&self.paths);
        if let Some(selected) = self.selected.clone() {
            self.expanded.extend(e1_ui::tree::ancestors(&selected));
        }
        self.rebuild_rows();
        self.rematch(cx);
    }

    /// Lay the tree out again after a fold, an unfold or a new tree.
    fn rebuild_rows(&mut self) {
        self.rows = self.tree.rows(&self.expanded);
    }

    /// Re-run the match after the query or the tree changed.
    fn rematch(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        self.matches = finder::find(&self.paths, &query, MATCH_CAP);
        cx.notify();
    }

    /// Whether the reader is searching rather than walking the tree.
    fn searching(&self, cx: &App) -> bool {
        !self.query.read(cx).value().trim().is_empty()
    }

    /// Fold or unfold a directory.
    fn toggle(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Read a file.
    fn open(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.selected = Some(path.clone());
        cx.emit(BrowserEvent::Open { repo, path });
        cx.notify();
    }

    /// One row of the tree: a directory that folds, or a file to read.
    fn tree_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx);
        let Some(node) = self.rows.get(index).and_then(|at| self.tree.node(*at)) else {
            return div().h(ROW_HEIGHT).into_any_element();
        };
        let open = self.expanded.contains(&node.path);
        let selected = !node.dir && self.selected.as_deref() == Some(node.path.as_str());
        let path = node.path.clone();
        let dir = node.dir;
        let icon = if dir {
            if open {
                IconName::FolderOpen
            } else {
                IconName::FolderClosed
            }
        } else {
            IconName::FileText
        };
        div()
            .h(ROW_HEIGHT)
            .w_full()
            .px_2()
            .child(
                h_flex()
                    .id(("node", index))
                    .size_full()
                    .pr_2()
                    .pl(px(4. + node.depth as f32 * INDENT))
                    .gap_1()
                    .items_center()
                    .rounded(px(tokens.radius.row))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(tokens.colors().row_active()))
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if dir {
                            this.toggle(path.clone(), cx)
                        } else {
                            this.open(path.clone(), cx)
                        }
                    }))
                    // A file sits where a directory's chevron would be, so
                    // the names of a folder's contents line up with it.
                    .child(div().w_3().flex_shrink_0().children(dir.then(|| {
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_3()
                        .text_color(tokens.colors().text_muted)
                    })))
                    .child(
                        Icon::new(icon)
                            .size_3p5()
                            .flex_shrink_0()
                            .text_color(if dir {
                                tokens.colors().accent
                            } else {
                                tokens.colors().text_muted
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl_1()
                            .font_family(mono)
                            .text_size(px(11.5))
                            .truncate()
                            .text_color(if selected || dir {
                                tokens.colors().text_primary
                            } else {
                                tokens.colors().text_secondary
                            })
                            .child(node.name.clone()),
                    ),
            )
            .into_any_element()
    }

    /// One row of the search: the whole path, the directory muted.
    fn match_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx);
        let Some(path) = self.matches.get(index).and_then(|i| self.paths.get(*i)) else {
            return div().h(ROW_HEIGHT).into_any_element();
        };
        let selected = self.selected.as_deref() == Some(path.as_str());
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (Some(format!("{dir}/")), name.to_string()),
            None => (None, path.clone()),
        };
        let path = path.clone();
        div()
            .h(ROW_HEIGHT)
            .w_full()
            .px_2()
            .child(
                h_flex()
                    .id(("path", index))
                    .size_full()
                    .px_2p5()
                    .gap_2()
                    .items_center()
                    .rounded(px(tokens.radius.row))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(tokens.colors().row_active()))
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(path.clone(), cx)))
                    .child(
                        Icon::new(IconName::FileText)
                            .size_3p5()
                            .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .overflow_hidden()
                            .font_family(mono)
                            .text_size(px(11.5))
                            .children(dir.map(|dir| {
                                div()
                                    .text_color(tokens.colors().text_muted)
                                    .truncate()
                                    .child(dir)
                            }))
                            .child(
                                div()
                                    .text_color(if selected {
                                        tokens.colors().text_primary
                                    } else {
                                        tokens.colors().text_secondary
                                    })
                                    .child(name),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn notice(&self, text: String, error: bool, cx: &App) -> AnyElement {
        let tokens = Tokens::global(cx);
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px_8()
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(if error {
                        tokens.colors().status_error
                    } else {
                        tokens.colors().text_muted
                    })
                    .text_center()
                    .child(text),
            )
            .into_any_element()
    }
}

impl Render for FileBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let error = self
            .repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).tree(repo))
            .and_then(|tree| tree.error())
            .map(str::to_string);
        let loading = self.is_loading(cx);
        let searching = self.searching(cx);

        let body: AnyElement = if self.paths.is_empty() {
            match error {
                Some(error) => self.notice(error, true, cx),
                None if loading => crate::skeleton::path_rows(10, cx),
                None => self.notice(rust_i18n::t!("files.empty").to_string(), false, cx),
            }
        } else if searching && self.matches.is_empty() {
            self.notice(rust_i18n::t!("files.empty").to_string(), false, cx)
        } else {
            let this = cx.entity();
            let count = if searching {
                self.matches.len()
            } else {
                self.rows.len()
            };
            uniform_list("paths", count, move |range, _window, cx| {
                this.update(cx, |this, cx| {
                    range
                        .map(|index| {
                            if searching {
                                this.match_row(index, mono.clone(), cx)
                            } else {
                                this.tree_row(index, mono.clone(), cx)
                            }
                        })
                        .collect()
                })
            })
            .flex_1()
            .size_full()
            .py_1()
            .into_any_element()
        };
        let body = match self.repo.clone() {
            Some(repo) if !self.paths.is_empty() => {
                crate::fade::fade_in(SharedString::from(format!("files:{repo}")), body, cx)
            }
            _ => body,
        };

        v_flex()
            .size_full()
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(Input::new(&self.query).cleanable(true)),
            )
            .when(self.truncated, |this| {
                this.child(
                    div()
                        .px_4()
                        .py_1()
                        .text_size(px(11.5))
                        .text_color(tokens.colors().status_attention)
                        .child(rust_i18n::t!("files.truncated").to_string()),
                )
            })
            .child(body)
    }
}
