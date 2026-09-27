//! The centre column as a repository's history: `docs/ui.md` §3.3.
//!
//! One row per commit, with the rail down the left that says which line of
//! development it sits on. The lanes are worked out in `e1_ui::graph`; what
//! is here is the drawing and the picking.

use crate::store::{Store, StoreEvent};
use e1_github::RepoId;
use e1_ui::{Tokens, graph};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex};

/// How tall a commit row is. Two lines and their air, as the item rows.
const ROW_HEIGHT: Pixels = px(52.);

/// How far apart the rail's lanes sit.
const LANE_WIDTH: f32 = 14.;

/// How wide the rail is before the message starts, whatever the lanes do.
const RAIL_MIN: f32 = 26.;

/// How many rows before the end of what has been read the next page is
/// asked for. Enough that the reader does not arrive at a stop.
const AHEAD: usize = 20;

/// Emitted when the reader picks a commit.
pub enum HistoryEvent {
    /// Read this commit.
    Open {
        /// Which repository.
        repo: RepoId,
        /// Which commit.
        sha: String,
    },
}

impl EventEmitter<HistoryEvent> for History {}

/// A repository's commits.
pub struct History {
    store: Entity<Store>,
    repo: Option<RepoId>,
    /// The rail, one row per commit, worked out when the history lands.
    rail: Vec<graph::Row>,
    /// How many lanes the rail uses.
    lanes: usize,
    selected: Option<String>,
    /// Open the newest commit as soon as there is one. What a launch
    /// argument asks for; a reader picks their own.
    open_newest: bool,
}

impl History {
    /// A history over a store, showing nothing until told which repository.
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&store, |this, _, _: &StoreEvent, cx| this.rebuild(cx))
            .detach();
        Self {
            store,
            repo: None,
            rail: Vec::new(),
            lanes: 1,
            selected: None,
            open_newest: false,
        }
    }

    /// Show a repository's history, fetching it if it never has been.
    pub fn set_repo(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.repo.as_ref() != Some(&repo) {
            self.selected = None;
        }
        self.repo = Some(repo.clone());
        self.store
            .update(cx, |store, cx| store.ensure_commits(repo, cx));
        self.rebuild(cx);
    }

    /// Read the newest commit as soon as the history lands.
    pub fn open_newest(&mut self, cx: &mut Context<Self>) {
        self.open_newest = true;
        self.rebuild(cx);
    }

    /// Which repository is on screen.
    pub fn repo(&self) -> Option<&RepoId> {
        self.repo.as_ref()
    }

    /// Fetch the history again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.clone() {
            self.store
                .update(cx, |store, cx| store.load_commits(repo, cx));
        }
    }

    /// Whether the history is being fetched.
    pub fn is_loading(&self, cx: &App) -> bool {
        self.repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).commits(repo))
            .is_some_and(|fetch| fetch.is_loading())
    }

    /// Lay the rail out again from the store's history.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let commits = self
            .repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).commits(repo))
            .and_then(|fetch| fetch.value());
        self.rail = match commits {
            Some(commits) => graph::lay_out(
                commits
                    .iter()
                    .map(|commit| (commit.sha.as_str(), commit.parents.as_slice())),
            ),
            None => Vec::new(),
        };
        self.lanes = graph::width(&self.rail).min(graph::MAX_LANES);
        let newest = self
            .open_newest
            .then(|| {
                self.repo
                    .as_ref()
                    .and_then(|repo| self.store.read(cx).commits(repo))
                    .and_then(|fetch| fetch.value())
                    .and_then(|commits| commits.first())
                    .map(|commit| commit.sha.clone())
            })
            .flatten();
        if let Some(sha) = newest {
            self.open_newest = false;
            self.open(sha, cx);
        }
        cx.notify();
    }

    fn open(&mut self, sha: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.selected = Some(sha.clone());
        cx.emit(HistoryEvent::Open { repo, sha });
        cx.notify();
    }

    /// The rail beside one row.
    ///
    /// Painted rather than built out of boxes: a thread that leaves one
    /// lane for another is a curve, and a curve is a path. Each row draws
    /// its own two halves, and they meet at the row's edges because both
    /// sides of that edge are the same lane at the same x.
    fn rail(&self, index: usize, cx: &App) -> AnyElement {
        let colors = *Tokens::global(cx).colors();
        let Some(row) = self.rail.get(index).cloned() else {
            return div().w(px(RAIL_MIN)).into_any_element();
        };
        let lanes = self.lanes;
        let width = (lanes as f32 * LANE_WIDTH).max(RAIL_MIN);
        let merge = row.merge;
        let lane = row.lane;
        div()
            .relative()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let x = |lane: usize| bounds.origin.x + px(8. + lane as f32 * LANE_WIDTH);
                        let top = bounds.origin.y;
                        let middle = bounds.origin.y + bounds.size.height / 2.;
                        let bottom = bounds.origin.y + bounds.size.height;
                        for segment in &row.segments {
                            if segment.from >= lanes || segment.to >= lanes {
                                continue;
                            }
                            let (from_y, to_y) = match segment.half {
                                graph::Half::Top => (top, middle),
                                graph::Half::Bottom => (middle, bottom),
                            };
                            window.paint_path(
                                thread(point(x(segment.from), from_y), point(x(segment.to), to_y)),
                                // A bend belongs to the branch, not to the
                                // trunk, so it takes the outer lane's colour.
                                lane_color(segment.from.max(segment.to), &colors),
                            );
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .when(lane < lanes, |this| {
                // The dot sits on the thread, filled for a plain commit and
                // ringed for a merge, which is what says two lines met here.
                this.child(
                    div()
                        .absolute()
                        .top(ROW_HEIGHT / 2. - px(4.))
                        .left(px(8. + lane as f32 * LANE_WIDTH) - px(4.))
                        .size(px(8.))
                        .rounded_full()
                        .when(merge, |this| {
                            this.border_2().border_color(lane_color(lane, &colors))
                        })
                        .bg(if merge {
                            colors.bg_window
                        } else {
                            lane_color(lane, &colors)
                        }),
                )
            })
            .into_any_element()
    }

    fn row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let Some(commit) = self
            .repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).commits(repo))
            .and_then(|fetch| fetch.value())
            .and_then(|commits| commits.get(index))
        else {
            return div().h(ROW_HEIGHT).into_any_element();
        };
        let selected = self.selected.as_deref() == Some(commit.sha.as_str());
        let subject = commit.subject().to_string();
        let short = commit.short().to_string();
        let author = commit.author_name.clone();
        let age = e1_ui::time::age(chrono::Utc::now(), commit.authored_at);
        let sha = commit.sha.clone();
        let rail = self.rail(index, cx);
        h_flex()
            .id(("commit", index))
            .h(ROW_HEIGHT)
            .w_full()
            .px_2()
            .items_center()
            .cursor_pointer()
            .when(selected, |this| this.bg(tokens.colors().row_active()))
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.open(sha.clone(), cx)))
            .child(rail)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .w_full()
                            .text_size(px(13.))
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(subject),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().text_muted)
                            .child(div().truncate().child(author))
                            .child(div().child(age))
                            .child(div().font_family(mono).child(short)),
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

impl Render for History {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let fetch = self
            .repo
            .as_ref()
            .and_then(|repo| self.store.read(cx).commits(repo));
        let error = fetch.and_then(|fetch| fetch.error()).map(str::to_string);
        let count = fetch
            .and_then(|fetch| fetch.value())
            .map(|commits| commits.len())
            .unwrap_or_default();
        let loading = self.is_loading(cx);

        if count == 0 {
            return v_flex()
                .size_full()
                .child(match error {
                    Some(error) => self.notice(error, true, cx),
                    None if loading => crate::skeleton::history_rows(10, cx),
                    None => self.notice(rust_i18n::t!("history.empty").to_string(), false, cx),
                })
                .into_any_element();
        }
        let this = cx.entity();
        let repo = self.repo.clone();
        v_flex()
            .size_full()
            .child(crate::fade::fade_in(
                SharedString::from(format!(
                    "history:{}",
                    repo.map(|repo| repo.to_string()).unwrap_or_default()
                )),
                uniform_list("commits", count, move |range, _window, cx| {
                    // Asking for the rows near the end is the reader having
                    // scrolled there. The next page is read after this
                    // layout rather than during it: a fetch begun mid-layout
                    // notifies into the frame it is part of.
                    let near_end = range.end + AHEAD >= count;
                    this.update(cx, |this, cx| {
                        if near_end && let Some(repo) = this.repo.clone() {
                            let store = this.store.clone();
                            cx.defer(move |cx| {
                                store.update(cx, |store, cx| store.more_commits(repo, cx));
                            });
                        }
                        range
                            .map(|index| this.row(index, mono.clone(), cx))
                            .collect()
                    })
                })
                .flex_1()
                .size_full()
                .py_1()
                .into_any_element(),
                cx,
            ))
            .into_any_element()
    }
}

/// What colour a lane's thread is.
///
/// One colour a lane, cycling: the trunk is the accent and the branches
/// beside it take the status hues, which is how a history says "this line
/// is not that line" without a legend. The palette is the theme's own, so a
/// lane cannot arrive at a colour the window does not already use.
fn lane_color(lane: usize, colors: &e1_ui::theme::Colors) -> Hsla {
    let palette = [
        colors.accent,
        colors.status_done,
        colors.status_working,
        colors.status_attention,
        colors.status_error,
        colors.text_muted,
    ];
    palette[lane % palette.len()]
}

/// One piece of thread, as a path.
///
/// `paint_path` fills; it does not stroke. A line is therefore a ribbon —
/// the thread and a copy of it a hair to the right — and it is built as a
/// run of small quads rather than two long curves, because each quad is
/// convex and fills exactly, while a long curved outline leaves the fill
/// rule to guess and it guesses a blob. At this size a dozen steps is a
/// curve to any eye.
///
/// A thread that changes lane is an S that stands vertically at both ends.
/// It has to: a bend meets a straight run of the same thread at the row's
/// edge, and it meets the dot at the row's middle, and a curve that arrives
/// sideways at either of those leaves a hook. Vertical at both ends is also
/// why the halves either side of a row's edge read as one line.
fn thread(from: Point<Pixels>, to: Point<Pixels>) -> Path<Pixels> {
    /// How thick a thread is.
    const WIDTH: f32 = 1.4;
    /// How many quads a bend is drawn with.
    const STEPS: usize = 16;
    /// How far along the drop the curve holds its lane before crossing.
    /// Half of it: less and the S is a corner, more and it is a wobble.
    const HOLD: f32 = 0.5;

    let half = px(WIDTH / 2.);
    let straight = from.x == to.x;
    let steps = if straight { 1 } else { STEPS };
    // A cubic, whose two control points sit above and below on the lanes
    // the thread leaves and joins. That is what makes both ends vertical.
    let drop = to.y - from.y;
    let first = point(from.x, from.y + drop * HOLD);
    let second = point(to.x, to.y - drop * HOLD);
    let at = |t: f32| {
        if straight {
            return point(from.x, from.y + drop * t);
        }
        let rest = 1. - t;
        let (a, b, c, d) = (
            rest * rest * rest,
            3. * rest * rest * t,
            3. * rest * t * t,
            t * t * t,
        );
        point(
            from.x * a + first.x * b + second.x * c + to.x * d,
            from.y * a + first.y * b + second.y * c + to.y * d,
        )
    };
    let mut path = Path::new(point(from.x - half, from.y));
    for step in 0..steps {
        let near = at(step as f32 / steps as f32);
        let far = at((step + 1) as f32 / steps as f32);
        path.move_to(point(near.x - half, near.y));
        path.line_to(point(far.x - half, far.y));
        path.line_to(point(far.x + half, far.y));
        path.line_to(point(near.x + half, near.y));
    }
    path
}
