//! The right column: `docs/ui.md` §3.4.
//!
//! One thing, read in full: an item — its header, its labels, assignees
//! and projects, the checks and the merge, its description and comments,
//! or its files and their diffs — or a file out of the tree. The long parts
//! are virtualized: a pull's diffs are one `uniform_list` of rows across
//! every file, and a file's lines are another, so a thousand-line diff
//! costs what the screen shows (`AGENTS.md` rule 7).
//!
//! The editable parts borrow GitHub's own shapes, because a reader who
//! knows those is not asked to learn ours: a facet is a heading with a
//! gear, and the gear opens a filterable list where a click adds or
//! removes; the merge is a card that says what the checks came to and
//! whether the branch conflicts, then a green button with the method on
//! it and the other methods behind a chevron.

use crate::avatar::avatar;
use crate::store::{FileKey, ItemKey, Store, StoreEvent};
use chrono::Utc;
use e1_github::{
    CheckState, Comment, FileStatus, JobStep, MergeMethod, Project, ProjectItem, ProjectItemKind,
    ProjectView, ProjectViewLayout, PullFile, RepoId, ReviewComment, ReviewEvent, Side,
};
use e1_ui::Tokens;
use e1_ui::diff;
use e1_ui::project::{RoadmapRow, board_columns, roadmap, visible_item_indices};
use e1_ui::rows::{Glyph, LabelChip};
use e1_ui::time::age;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::text::TextView;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};
use std::collections::HashSet;

/// A turning spinner for what is still running: the toolkit's, on the
/// loader glyph, so a check in progress moves instead of sitting there.
fn spinner(size: Pixels, color: Hsla) -> AnyElement {
    use gpui_component::Sizable as _;
    gpui_component::spinner::Spinner::new()
        .icon(IconName::LoaderCircle)
        .with_size(size)
        .color(color)
        .into_any_element()
}

/// How often what is still running on screen is asked about again.
const POLL_SECONDS: u64 = 20;

/// The reading measure, in pixels. Long-form text stays readable because the
/// column stops growing, not because the window does.
const MEASURE: f32 = 720.;

/// How tall a diff row is: a file's header and a line of it are the same
/// height, which is what lets every file's diff be one virtualized list.
const DIFF_ROW: Pixels = px(22.);

/// How tall a line of a file is.
const CODE_ROW: Pixels = px(20.);

/// How tall one item in a Project is.
const PROJECT_ROW: Pixels = px(60.);

/// A fixed-height Kanban card; two title lines fit without abandoning list
/// virtualization.
const PROJECT_CARD: Pixels = px(82.);

/// Which half of a pull is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// The description and the comments.
    Conversation,
    /// The files and their diffs.
    Files,
}

/// What the column tells the window about.
pub enum DetailEvent {
    /// Open the far-right agent pane on this context.
    Ask(e1_ui::agents::Ask),
}

impl EventEmitter<DetailEvent> for Detail {}

/// What the column is reading.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Showing {
    /// A pull or an issue.
    Item(ItemKey),
    /// A file out of a repository's tree.
    File(FileKey),
    /// The log of an Actions job.
    Log {
        repo: RepoId,
        job: u64,
        name: String,
    },
    /// One commit out of a repository's history.
    Commit { repo: RepoId, sha: String },
    /// A GitHub Project and its items.
    Project(Project),
}

/// A Project card while it is being moved between Kanban columns.
#[derive(Clone)]
struct DraggedProjectItem {
    project: Project,
    item_id: String,
    field_id: String,
    source_value: Option<String>,
    title: SharedString,
}

impl Render for DraggedProjectItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        div()
            .w(px(280.))
            .px_3()
            .py_2()
            .rounded(px(tokens.radius.row))
            .border_1()
            .border_color(tokens.colors().accent)
            .bg(tokens.colors().bg_surface)
            .shadow_md()
            .text_size(px(12.5))
            .text_color(tokens.colors().text_primary)
            .child(self.title.clone())
    }
}

/// One row of the log screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogRow {
    /// A step's heading: fold it or unfold it.
    Step(usize),
    /// A line of the log, under an unfolded step — or on its own when the
    /// job reported no steps to put it under.
    Line(usize),
    /// The job reported no steps, so the log is shown whole.
    NoSteps,
}

/// One row of the diff list.
enum DiffRow {
    /// A file's header: status, path, counts. Picking it folds the diff.
    File {
        index: usize,
        name: SharedString,
        status: FileStatus,
        additions: u64,
        deletions: u64,
        collapsed: bool,
    },
    /// A line of a diff, in the unified view. Clicking it opens a comment.
    Line { path: String, line: diff::Line },
    /// Two lines side by side, in the split view.
    Pair {
        path: String,
        left: Option<diff::Line>,
        right: Option<diff::Line>,
    },
    /// A hunk header in the split view.
    Hunk(SharedString),
    /// A comment someone left on the line above.
    Comment(ReviewComment),
    /// The comment being written on the line above.
    Composer,
    /// A file with nothing to show under it.
    Note(SharedString),
}

/// One of the three lists a reader can edit from the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picker {
    /// The repository's labels.
    Labels,
    /// The repository's assignable people.
    Assignees,
    /// The owner's projects.
    Projects,
}

/// The right column.
pub struct Detail {
    store: Entity<Store>,
    showing: Option<Showing>,
    /// The saved Project view selected in the Project header.
    project_view: Option<String>,
    /// The Project and saved view an item card was opened from.
    project_return: Option<(Project, Option<String>)>,
    tab: Tab,
    /// The files whose diffs are folded. Everything starts open: the list is
    /// virtualized, so a hundred files cost nothing until they are scrolled
    /// to, and a review reads top to bottom.
    collapsed: HashSet<String>,
    diff_rows: Vec<DiffRow>,
    /// A file's lines, split once when it lands.
    lines: Vec<SharedString>,
    /// The comment being written.
    composer: Entity<TextareaState>,
    /// Empty the composer at the next frame: clearing needs the window,
    /// which the answer that asks for it does not have.
    clear_composer: bool,
    /// The picker that is open under its facet, if one is.
    picker: Option<Picker>,
    /// What is typed into the open picker.
    filter: Entity<InputState>,
    /// Empty the filter at the next frame, when a picker opens.
    clear_filter: bool,
    /// The list of merge methods is open under the merge button.
    merge_menu: bool,
    /// How the next merge is done.
    merge_method: MergeMethod,
    /// The merge button was pressed once; the next press merges.
    confirm_merge: bool,
    /// The checks card is unfolded to its runs.
    /// Whether the checks card's runs are unfolded. Folded by default: the
    /// summary line says what matters, and the runs are a press away.
    checks_open: bool,
    /// The diff's rows, virtualized with variable heights: a comment is
    /// taller than a line.
    diff_state: ListState,
    /// Side by side rather than unified.
    split: bool,
    /// The line a comment is being written on, if one is.
    composing: Option<(String, u32, Side)>,
    /// The first line of the range the comment is on, when a second line
    /// was picked with ⇧ held; the range ends at the composing line.
    compose_start: Option<u32>,
    /// The comment being written on a line.
    review_input: Entity<TextareaState>,
    /// Empty the line composer at the next frame.
    clear_review: bool,
    /// The file on screen, parsed for highlighting: `None` when there is
    /// no grammar for it, or more of it than is worth parsing.
    code: Option<e1_ui::code::Code>,
    /// The lines of the log the reader has picked out, as row indices.
    log_selection: Option<(usize, usize)>,
    /// Text picked out of what is rendered — a comment, a body.
    picked_text: Option<String>,
    /// Lines picked out of the file on screen.
    code_selection: Option<(usize, usize)>,
    /// Lines picked out of the diff: which file, and the rows either end.
    diff_selection: Option<(String, usize, usize)>,
    /// Where the pointer let go of a pick, which is where the offer goes.
    offer_at: Option<Point<Pixels>>,
    /// Open the ask at the next frame, which is the first place with a
    /// window to open it from.
    ask_soon: bool,
    /// Pick lines and open the box the moment there are lines. A launch
    /// argument asks for this; a reader picks their own.
    ask_when_ready: bool,
    /// What the review controls have to say for themselves, when a review
    /// could not be sent as asked.
    review_says: Option<String>,
    /// A job's log, parsed once when it lands.
    log_lines: Vec<e1_ui::log::Line>,
    /// What was on screen before the log, to go back to.
    log_previous: Option<Showing>,
    /// The job's steps, once they land.
    log_steps: Vec<JobStep>,
    /// Which step each log line falls under.
    log_owner: Vec<usize>,
    /// The steps that are unfolded, by number.
    log_open: HashSet<u64>,
    /// Whether the reader has folded or unfolded a step. Until they do,
    /// the steps that failed, or are still running, unfold themselves.
    log_touched: bool,
    /// Whether the job answered without any steps.
    log_flat: bool,
    /// The log screen's rows, in order.
    log_rows: Vec<LogRow>,
    /// The log's list, which remembers each row's height so that long lines
    /// can wrap.
    log_state: ListState,
}

impl Detail {
    /// A detail over a store, showing nothing until told what to.
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&store, |this, _, _: &StoreEvent, cx| this.rebuild(cx))
            .detach();
        // A check that is running finishes without telling us, so what is
        // on screen and still pending is asked about again every so often.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(POLL_SECONDS))
                    .await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("detail.comment.placeholder").to_string())
                .auto_grow(2, 8)
        });
        cx.subscribe(&composer, |this, _, event: &InputEvent, cx| {
            // ⌘⏎ sends, the way it does on GitHub; a plain ⏎ is a newline.
            match event {
                InputEvent::PressEnter {
                    secondary: true, ..
                } => this.send_comment(cx),
                InputEvent::Change if this.review_says.is_some() => {
                    this.review_says = None;
                    cx.notify();
                }
                _ => {}
            }
        })
        .detach();
        let filter = cx.new(|cx| InputState::new(window, cx));
        let review_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("diff.comment.placeholder").to_string())
                .auto_grow(2, 6)
        });
        cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        Self {
            store,
            showing: None,
            project_view: None,
            project_return: None,
            tab: Tab::Conversation,
            collapsed: HashSet::new(),
            diff_rows: Vec::new(),
            lines: Vec::new(),
            composer,
            clear_composer: false,
            picker: None,
            filter,
            clear_filter: false,
            merge_menu: false,
            merge_method: MergeMethod::default(),
            confirm_merge: false,
            checks_open: false,
            diff_state: ListState::new(0, ListAlignment::Top, px(300.)),
            split: false,
            composing: None,
            compose_start: None,
            review_input,
            clear_review: false,
            code: None,
            log_selection: None,
            picked_text: None,
            code_selection: None,
            diff_selection: None,
            offer_at: None,
            ask_soon: false,
            ask_when_ready: false,
            review_says: None,
            log_lines: Vec::new(),
            log_previous: None,
            log_steps: Vec::new(),
            log_owner: Vec::new(),
            log_open: HashSet::new(),
            log_touched: false,
            log_flat: false,
            log_rows: Vec::new(),
            log_state: ListState::new(0, ListAlignment::Top, px(300.)),
        }
    }

    /// Show an Actions job's log, remembering what was on screen so the
    /// reader can go back to it.
    pub fn show_log(&mut self, repo: RepoId, job: u64, name: String, cx: &mut Context<Self>) {
        if !matches!(self.showing, Some(Showing::Log { .. })) {
            self.log_previous = self.showing.clone();
        }
        self.showing = Some(Showing::Log {
            repo: repo.clone(),
            job,
            name,
        });
        self.log_open.clear();
        self.log_touched = false;
        self.store.update(cx, |store, cx| {
            store.ensure_log(repo.clone(), job, cx);
            store.ensure_job(repo, job, cx);
        });
        self.rebuild(cx);
    }

    /// Pick a line of the log, or stretch the pick to it with ⇧ held.
    fn pick_log_line(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        self.log_selection = match (self.log_selection, extend) {
            (Some((anchor, _)), true) => Some((anchor.min(index), anchor.max(index))),
            _ => Some((index, index)),
        };
        cx.notify();
    }

    /// Whether a row of the log is inside the pick.
    fn log_picked(&self, index: usize) -> bool {
        self.log_selection
            .is_some_and(|(from, to)| (from..=to).contains(&index))
    }

    /// Notice what the reader dragged over.
    ///
    /// A pointer let go is the only moment a selection is finished, and the
    /// toolkit keeps the selection for the whole window rather than per
    /// view, so this is the one place that has to ask.
    fn notice_selection(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let picked = gpui_base::TextSelection::selected_text(window, cx);
        let picked = picked.trim();
        self.picked_text = (!picked.is_empty()).then(|| picked.to_string());
        // A pick is a pick however it was made: dragged over rendered text,
        // or clicked down a column of lines. The offer follows the pointer
        // either way, and goes when there is nothing picked.
        let anything = self.picked_text.is_some()
            || self.log_selection.is_some()
            || self.code_selection.is_some()
            || self.diff_selection.is_some();
        self.offer_at = anything.then_some(at);
        cx.notify();
    }

    /// Pick a line of the file, or stretch the pick to it with ⇧ held.
    fn pick_code_line(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        self.code_selection = match (self.code_selection, extend) {
            (Some((anchor, _)), true) => Some((anchor.min(index), anchor.max(index))),
            _ => Some((index, index)),
        };
        cx.notify();
    }

    /// Whether a row of the file is inside the pick.
    fn code_picked(&self, index: usize) -> bool {
        self.code_selection
            .is_some_and(|(from, to)| (from..=to).contains(&index))
    }

    /// Pick a row of the diff for the ask, or stretch the pick to it.
    ///
    /// With ⌥ held, because a plain click on a diff line already means
    /// "comment on this line" and one gesture cannot mean two things.
    fn pick_diff_row(&mut self, path: String, index: usize, extend: bool, cx: &mut Context<Self>) {
        self.diff_selection = match (&self.diff_selection, extend) {
            (Some((picked, anchor, _)), true) if *picked == path => {
                Some((path, (*anchor).min(index), (*anchor).max(index)))
            }
            _ => Some((path, index, index)),
        };
        cx.notify();
    }

    /// Whether a row of the diff is inside the pick.
    fn diff_picked(&self, path: &str, index: usize) -> bool {
        self.diff_selection
            .as_ref()
            .is_some_and(|(picked, from, to)| picked == path && (*from..=*to).contains(&index))
    }

    /// The offer that appears where a selection was let go: a chip that
    /// opens the ask on what was picked.
    fn picked_offer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Nothing to offer when there is nowhere to send it.
        if !self.store.read(cx).has_chat_agent() {
            return None;
        }
        let at = self.offer_at?;
        let tokens = Tokens::global(cx).clone();
        Some(
            deferred(
                // The point came from a mouse event, so it is in the
                // window's coordinates. Anchored against the column, the
                // chip landed a column's width from the pointer.
                anchored()
                    .position(point(at.x + px(8.), at.y + px(12.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        h_flex()
                            .id("picked-offer")
                            .px_2()
                            .py_1()
                            .gap_1p5()
                            .items_center()
                            .rounded(px(tokens.radius.control()))
                            .bg(tokens.colors().popover())
                            .border_1()
                            .border_color(tokens.colors().border_strong)
                            .shadow_lg()
                            .cursor_pointer()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().accent)
                            .child(Icon::new(IconName::Bot).size_3())
                            .child(rust_i18n::t!("ask.button").to_string())
                            .on_click(cx.listener(|this, _, window, cx| this.open_ask(window, cx))),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    /// What the ask box would send, from what is on screen and picked.
    fn ask(&self, cx: &App) -> Option<e1_ui::agents::Ask> {
        let store = self.store.read(cx);
        let question = String::new();
        match self.showing.as_ref()? {
            Showing::Item(key) => {
                let detail = store.detail(key).and_then(|fetch| fetch.value())?;
                let item = &detail.item;
                // The facts are the point of doing this here rather than
                // leaving the reader to paste a link: what e1 knows — the
                // owner, the number, the branch, the head commit — is what
                // an agent needs to read the rest for itself.
                let mut facts = vec![
                    ("repository".to_string(), key.0.to_string()),
                    ("owner".to_string(), key.0.owner.clone()),
                    (
                        if item.is_pull() {
                            "pull request"
                        } else {
                            "issue"
                        }
                        .to_string(),
                        format!("#{}", item.number),
                    ),
                ];
                if let Some(pull) = &detail.pull {
                    facts.push((
                        "branch".to_string(),
                        format!("{} \u{2192} {}", pull.head, pull.base),
                    ));
                    if !pull.head_sha.is_empty() {
                        facts.push(("head commit".to_string(), pull.head_sha.clone()));
                    }
                }
                if !item.labels.is_empty() {
                    facts.push((
                        "labels".to_string(),
                        item.labels
                            .iter()
                            .map(|label| label.name.clone())
                            .collect::<Vec<_>>()
                            .join(", "),
                    ));
                }
                // A pick beats the whole body: the reader went to the
                // trouble of saying which part they meant. Lines out of the
                // diff say it most particularly of all.
                let from_diff = self.diff_selection.as_ref().map(|(path, from, to)| {
                    let text: Vec<String> = self
                        .diff_rows
                        .get(*from..=*to)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|row| match row {
                            DiffRow::Line { line, .. } => Some(line.text.to_string()),
                            _ => None,
                        })
                        .collect();
                    (path.clone(), text.join("\n"))
                });
                if let Some((path, text)) = from_diff.filter(|(_, text)| !text.trim().is_empty()) {
                    facts.push(("file".to_string(), path.clone()));
                    return Some(e1_ui::agents::Ask {
                        repo: Some(key.0.clone()),
                        subject: format!("#{} {}", item.number, item.title),
                        url: Some(item.html_url.clone()),
                        source: Some(rust_i18n::t!("ask.diff_source", path = path).to_string()),
                        excerpt: Some(text),
                        question,
                        facts,
                    });
                }
                let picked = self.picked_text.clone();
                let source = match picked {
                    Some(_) => rust_i18n::t!("ask.picked_source"),
                    None => rust_i18n::t!("ask.item_source"),
                };
                let excerpt = picked
                    .or_else(|| Some(item.body.clone()))
                    .filter(|text| !text.trim().is_empty());
                Some(e1_ui::agents::Ask {
                    repo: Some(key.0.clone()),
                    subject: format!("#{} {}", item.number, item.title),
                    url: Some(item.html_url.clone()),
                    source: excerpt.as_ref().map(|_| source.to_string()),
                    excerpt,
                    question,
                    facts,
                })
            }
            Showing::Log { repo, job, name } => {
                let (from, to) = self.log_selection?;
                let excerpt: Vec<String> = self
                    .log_lines
                    .get(from..=to.min(self.log_lines.len().saturating_sub(1)))
                    .unwrap_or_default()
                    .iter()
                    .map(|line| line.text.clone())
                    .collect();
                let read = store.job(repo, *job).and_then(|fetch| fetch.value());
                let mut facts = vec![
                    ("repository".to_string(), repo.to_string()),
                    ("owner".to_string(), repo.owner.clone()),
                    ("job".to_string(), format!("{name} ({job})")),
                ];
                if let Some(run) = read.map(|job| job.run_id).filter(|run| *run != 0) {
                    facts.push(("workflow run".to_string(), run.to_string()));
                }
                if let Some(failed) = read.and_then(|job| {
                    job.steps
                        .iter()
                        .find(|step| step.state == CheckState::Failure)
                }) {
                    facts.push(("failed step".to_string(), failed.name.clone()));
                }
                // A log is almost always opened from a pull, and that pull
                // is what the reader is actually working on.
                if let Some(Showing::Item(from)) = &self.log_previous {
                    facts.push(("opened from".to_string(), format!("#{}", from.1)));
                }
                Some(e1_ui::agents::Ask {
                    repo: Some(repo.clone()),
                    subject: rust_i18n::t!("log.title", id = job).to_string(),
                    url: read.map(|job| job.html_url.clone()),
                    source: Some(
                        rust_i18n::t!(
                            "ask.log_source",
                            job = name.clone(),
                            from = from + 1,
                            to = to + 1
                        )
                        .to_string(),
                    ),
                    excerpt: Some(excerpt.join("\n")),
                    question,
                    facts,
                })
            }
            Showing::File(key) => {
                let (from, to) = self.code_selection?;
                let last = self.lines.len().saturating_sub(1);
                let excerpt: Vec<String> = self
                    .lines
                    .get(from..=to.min(last))
                    .unwrap_or_default()
                    .iter()
                    .map(|line| line.to_string())
                    .collect();
                Some(e1_ui::agents::Ask {
                    repo: Some(key.0.clone()),
                    subject: key.1.clone(),
                    url: store
                        .content(key)
                        .and_then(|fetch| fetch.value())
                        .map(|content| content.html_url.clone()),
                    source: Some(
                        rust_i18n::t!(
                            "ask.file_source",
                            path = key.1.clone(),
                            from = from + 1,
                            to = to + 1
                        )
                        .to_string(),
                    ),
                    excerpt: Some(excerpt.join("\n")),
                    question,
                    facts: vec![
                        ("repository".to_string(), key.0.to_string()),
                        ("owner".to_string(), key.0.owner.clone()),
                        ("file".to_string(), key.1.clone()),
                        ("lines".to_string(), format!("{}-{}", from + 1, to + 1)),
                    ],
                })
            }
            Showing::Commit { .. } | Showing::Project(_) => None,
        }
    }

    /// Pick a few lines and open the ask box as soon as there are lines to
    /// pick, for screenshots (`E1_DEMO_ASK=1`).
    pub fn ask_at_launch(&mut self, cx: &mut Context<Self>) {
        self.ask_when_ready = true;
        self.rebuild(cx);
    }

    /// Leave the log for whatever was on screen before it.
    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.log_previous.take() {
            self.showing = Some(previous);
            self.rebuild(cx);
        }
    }

    /// Fold or unfold a step of the log.
    fn toggle_step(&mut self, number: u64, cx: &mut Context<Self>) {
        self.log_touched = true;
        if !self.log_open.remove(&number) {
            self.log_open.insert(number);
        }
        self.rebuild_log_rows();
        cx.notify();
    }

    /// Lay the log screen out again: a heading per step, and under each
    /// unfolded one the lines that were written while it ran.
    fn rebuild_log_rows(&mut self) {
        self.log_rows.clear();
        if self.log_steps.is_empty() {
            if self.log_flat && !self.log_lines.is_empty() {
                self.log_rows.push(LogRow::NoSteps);
            }
            self.log_rows
                .extend((0..self.log_lines.len()).map(LogRow::Line));
        } else {
            for (index, step) in self.log_steps.iter().enumerate() {
                self.log_rows.push(LogRow::Step(index));
                if self.log_open.contains(&step.number) {
                    self.log_rows.extend(
                        self.log_owner
                            .iter()
                            .enumerate()
                            .filter(|(_, owner)| **owner == index)
                            .map(|(line, _)| LogRow::Line(line)),
                    );
                }
            }
        }
        self.log_state.reset(self.log_rows.len());
    }

    fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        self.split = split;
        self.rebuild(cx);
    }

    /// Open a comment box under a line of the diff. With ⇧ held while a
    /// box is open on the same file and side, the comment covers the lines
    /// between the two instead, the way it does on GitHub.
    fn start_line_comment(
        &mut self,
        path: String,
        line: u32,
        side: Side,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        match &self.composing {
            Some((open_path, open_line, open_side))
                if extend && *open_path == path && *open_side == side =>
            {
                let anchor = self.compose_start.unwrap_or(*open_line);
                let (start, end) = if line < anchor {
                    (line, anchor)
                } else {
                    (anchor, line)
                };
                self.compose_start = Some(start).filter(|start| *start < end);
                self.composing = Some((path, end, side));
            }
            _ => {
                self.composing = Some((path, line, side));
                self.compose_start = None;
                self.clear_review = true;
            }
        }
        self.rebuild(cx);
    }

    fn cancel_line_comment(&mut self, cx: &mut Context<Self>) {
        self.composing = None;
        self.compose_start = None;
        self.rebuild(cx);
    }

    /// Whether a line of the diff is inside the range being commented on.
    fn in_compose_range(&self, path: &str, line: &diff::Line) -> bool {
        let Some((open_path, end, side)) = &self.composing else {
            return false;
        };
        if open_path != path {
            return false;
        }
        let number = match side {
            Side::Left => line.old,
            Side::Right => line.new,
        };
        let start = self.compose_start.unwrap_or(*end);
        number.is_some_and(|number| (start..=*end).contains(&number))
    }

    /// Ask again about whatever on screen is still running, so a spinner
    /// stops when the work does. Called on a timer.
    fn poll(&mut self, cx: &mut Context<Self>) {
        match self.showing.clone() {
            Some(Showing::Item(key)) => {
                let Some(sha) = self.head_sha(cx) else {
                    return;
                };
                let store = self.store.read(cx);
                let running = store.checks(&key.0, &sha).is_some_and(|fetch| {
                    !fetch.is_loading()
                        && fetch.value().is_some_and(|checks| {
                            checks
                                .runs
                                .iter()
                                .any(|run| run.state == CheckState::Pending)
                        })
                });
                if running {
                    self.store
                        .update(cx, |store, cx| store.load_checks(key.0, sha, cx));
                }
            }
            Some(Showing::Log { repo, job, .. }) => {
                let store = self.store.read(cx);
                let running = store.job(&repo, job).is_some_and(|fetch| {
                    !fetch.is_loading()
                        && fetch.value().is_none_or(|job| {
                            job.state == CheckState::Pending
                                || job
                                    .steps
                                    .iter()
                                    .any(|step| step.state == CheckState::Pending)
                        })
                });
                if running {
                    self.store.update(cx, |store, cx| {
                        store.load_job(repo.clone(), job, cx);
                        store.load_log(repo, job, cx);
                    });
                }
            }
            _ => {}
        }
    }

    /// Post the line comment at the pull's head.
    fn send_line_comment(&mut self, cx: &mut Context<Self>) {
        let (Some(key), Some((path, line, side))) = (self.item_key(), self.composing.clone())
        else {
            return;
        };
        let body = self.review_input.read(cx).value().trim().to_string();
        if body.is_empty() {
            return;
        }
        let Some(commit) = self.head_sha(cx) else {
            return;
        };
        let start = self.compose_start.take();
        self.composing = None;
        self.clear_review = true;
        self.store.update(cx, |store, cx| {
            store.review_comment(key, commit, path, start, line, side, body, cx)
        });
        self.rebuild(cx);
    }

    fn set_draft(&mut self, draft: bool, cx: &mut Context<Self>) {
        if let Some(key) = self.item_key() {
            self.store
                .update(cx, |store, cx| store.set_draft(key, draft, cx));
            cx.notify();
        }
    }

    fn item_key(&self) -> Option<ItemKey> {
        match &self.showing {
            Some(Showing::Item(key)) => Some(key.clone()),
            _ => None,
        }
    }

    /// Post what is in the composer.
    fn send_comment(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.item_key() else {
            return;
        };
        let body = self.composer.read(cx).value().trim().to_string();
        if body.is_empty() {
            return;
        }
        self.clear_composer = true;
        self.store
            .update(cx, |store, cx| store.comment_on(key, body, cx));
        cx.notify();
    }

    /// Submit a review with what is in the composer as its body. An
    /// approval needs no words; a request for changes reads better with
    /// some, but GitHub accepts either.
    fn send_review(&mut self, event: ReviewEvent, cx: &mut Context<Self>) {
        let Some(key) = self.item_key() else {
            return;
        };
        let body = self.composer.read(cx).value().trim().to_string();
        // GitHub refuses a request for changes with nothing said, and a
        // round trip to be told so is a click that looks like it did
        // nothing. Approving says everything it needs to by itself.
        if event == ReviewEvent::RequestChanges && body.is_empty() {
            self.review_says = Some(rust_i18n::t!("detail.review.needs_body").to_string());
            cx.notify();
            return;
        }
        self.review_says = None;
        self.clear_composer = true;
        self.store
            .update(cx, |store, cx| store.review(key, event, body, cx));
        cx.notify();
    }

    fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(key) = self.item_key() {
            self.store
                .update(cx, |store, cx| store.set_open(key, open, cx));
            cx.notify();
        }
    }

    /// The merge button: the first press arms it, the second merges. A
    /// merge is the one thing here git cannot take back.
    fn press_merge(&mut self, cx: &mut Context<Self>) {
        self.merge_menu = false;
        if !self.confirm_merge {
            self.confirm_merge = true;
            cx.notify();
            return;
        }
        self.confirm_merge = false;
        let method = self.merge_method;
        if let Some(key) = self.item_key() {
            self.store
                .update(cx, |store, cx| store.merge(key, method, cx));
            cx.notify();
        }
    }

    /// Open one of the pickers, fetching what it lists, or close it.
    fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        if self.picker == Some(picker) {
            self.picker = None;
            cx.notify();
            return;
        }
        self.picker = Some(picker);
        self.clear_filter = true;
        if let Some(key) = self.item_key() {
            let repo = key.0.clone();
            self.store.update(cx, |store, cx| match picker {
                Picker::Labels => store.ensure_repo_labels(repo, cx),
                Picker::Assignees => store.ensure_candidates(repo, cx),
                Picker::Projects => {
                    store.ensure_projects(repo.owner.clone(), cx);
                    store.ensure_memberships(key, cx);
                }
            });
        }
        cx.notify();
    }

    fn toggle_label(&mut self, name: String, has: bool, cx: &mut Context<Self>) {
        if let Some(key) = self.item_key() {
            self.store.update(cx, |store, cx| {
                if has {
                    store.remove_label(key, name, cx)
                } else {
                    store.add_label(key, name, cx)
                }
            });
        }
    }

    fn toggle_assignee(&mut self, login: String, has: bool, cx: &mut Context<Self>) {
        if let Some(key) = self.item_key() {
            self.store.update(cx, |store, cx| {
                if has {
                    store.remove_assignee(key, login, cx)
                } else {
                    store.add_assignee(key, login, cx)
                }
            });
        }
    }

    fn toggle_project(
        &mut self,
        project_id: String,
        item_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = self.item_key() {
            self.store.update(cx, |store, cx| match item_id {
                Some(item_id) => store.remove_from_project(key, project_id, item_id, cx),
                None => store.add_to_project(key, project_id, cx),
            });
        }
    }

    /// Show an item, fetching it if it never has been.
    pub fn show(&mut self, key: ItemKey, is_pull: Option<bool>, cx: &mut Context<Self>) {
        self.project_return = None;
        self.show_item(key, is_pull, cx);
    }

    /// Show an item reached from a Project while remembering the exact saved
    /// view the reader should return to.
    fn show_project_item(
        &mut self,
        project: Project,
        key: ItemKey,
        is_pull: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        self.project_return = Some((project, self.project_view.clone()));
        self.show_item(key, is_pull, cx);
    }

    fn show_item(&mut self, key: ItemKey, is_pull: Option<bool>, cx: &mut Context<Self>) {
        let showing = Showing::Item(key.clone());
        if self.showing.as_ref() != Some(&showing) {
            self.tab = Tab::Conversation;
            self.collapsed.clear();
            self.confirm_merge = false;
            self.merge_menu = false;
            self.picker = None;
            self.checks_open = false;
            self.composing = None;
            self.code = None;
        }
        self.showing = Some(showing);
        self.store
            .update(cx, |store, cx| store.ensure_detail(key, is_pull, cx));
        self.rebuild(cx);
    }

    /// Show a Project and fetch its items inside the app.
    pub fn show_project(&mut self, project: Project, cx: &mut Context<Self>) {
        self.project_return = None;
        if !matches!(&self.showing, Some(Showing::Project(shown)) if shown.id == project.id) {
            self.project_view = None;
        }
        self.showing = Some(Showing::Project(project.clone()));
        self.store
            .update(cx, |store, cx| store.ensure_project(project, cx));
        self.rebuild(cx);
    }

    /// Return from an Issue or Pull Request to the Project card that opened it.
    fn return_to_project(&mut self, cx: &mut Context<Self>) {
        let Some((project, view)) = self.project_return.take() else {
            return;
        };
        self.project_view = view;
        self.showing = Some(Showing::Project(project.clone()));
        self.store
            .update(cx, |store, cx| store.ensure_project(project, cx));
        self.rebuild(cx);
    }

    /// Show a commit, fetching it if it never has been.
    pub fn show_commit(&mut self, repo: RepoId, sha: String, cx: &mut Context<Self>) {
        let showing = Showing::Commit {
            repo: repo.clone(),
            sha: sha.clone(),
        };
        if self.showing.as_ref() != Some(&showing) {
            self.collapsed.clear();
            self.code = None;
        }
        self.showing = Some(showing);
        self.store
            .update(cx, |store, cx| store.ensure_commit(repo, sha, cx));
        self.rebuild(cx);
    }

    /// Show a file, fetching it if it never has been.
    pub fn show_file(&mut self, key: FileKey, cx: &mut Context<Self>) {
        self.showing = Some(Showing::File(key.clone()));
        self.store
            .update(cx, |store, cx| store.ensure_content(key, cx));
        self.rebuild(cx);
    }

    /// Switch to the files of the pull on screen, with one of them singled
    /// out by folding the rest.
    pub fn show_files(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        if let Some(only) = only
            && let Some(key) = self.item_key()
        {
            let files = self
                .store
                .read(cx)
                .pull_files(&key)
                .and_then(|fetch| fetch.value())
                .cloned()
                .unwrap_or_default();
            self.collapsed = files
                .iter()
                .map(|file| file.filename.clone())
                .filter(|name| name != &only)
                .collect();
        }
        self.set_tab(Tab::Files, cx);
    }

    /// Fetch again whatever is on screen.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.showing.clone() {
            Some(Showing::Item(key)) => {
                let files = self.tab == Tab::Files;
                let sha = self.head_sha(cx);
                self.store.update(cx, |store, cx| {
                    store.load_detail(key.clone(), None, cx);
                    if files {
                        store.load_pull_files(key.clone(), cx);
                    }
                    if let Some(sha) = sha {
                        store.load_checks(key.0.clone(), sha, cx);
                    }
                });
            }
            Some(Showing::File(key)) => {
                self.store
                    .update(cx, |store, cx| store.load_content(key, cx));
            }
            Some(Showing::Log { repo, job, .. }) => {
                self.store.update(cx, |store, cx| {
                    store.load_log(repo.clone(), job, cx);
                    store.load_job(repo, job, cx);
                });
            }
            Some(Showing::Commit { repo, sha }) => {
                self.store
                    .update(cx, |store, cx| store.load_commit(repo, sha, cx));
            }
            Some(Showing::Project(project)) => {
                self.store
                    .update(cx, |store, cx| store.load_project(project, cx));
            }
            None => {}
        }
    }

    /// Where what is on screen lives on the web, when it is known.
    pub fn html_url(&self, cx: &App) -> Option<String> {
        let store = self.store.read(cx);
        match self.showing.as_ref()? {
            Showing::Item(key) => store
                .detail(key)
                .and_then(|detail| detail.value())
                .map(|detail| detail.item.html_url.clone()),
            Showing::File(key) => store
                .content(key)
                .and_then(|content| content.value())
                .map(|content| content.html_url.clone()),
            Showing::Commit { repo, sha } => store
                .commit(repo, sha)
                .and_then(|fetch| fetch.value())
                .map(|detail| detail.commit.html_url.clone()),
            Showing::Log { repo, job, .. } => store
                .job(repo, *job)
                .and_then(|job| job.value())
                .map(|job| job.html_url.clone()),
            Showing::Project(project) => Some(project.html_url.clone()),
        }
    }

    /// The head commit of the pull on screen, once the detail has landed.
    fn head_sha(&self, cx: &App) -> Option<String> {
        let key = self.item_key()?;
        self.store
            .read(cx)
            .detail(&key)
            .and_then(|fetch| fetch.value())
            .and_then(|detail| detail.pull.as_ref())
            .map(|pull| pull.head_sha.clone())
            .filter(|sha| !sha.is_empty())
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Files
            && let Some(key) = self.item_key()
        {
            self.store.update(cx, |store, cx| {
                store.ensure_pull_files(key.clone(), cx);
                store.ensure_review_comments(key, cx);
            });
        }
        self.rebuild(cx);
    }

    fn toggle_file(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&name) {
            self.collapsed.insert(name);
        }
        self.rebuild(cx);
    }

    /// The virtualized rows for a set of files and the comments on them.
    ///
    /// A pull's files and a commit's files are the same thing to a reader,
    /// so they are the same thing here: a commit simply has no comments to
    /// hang under its lines.
    fn diff_rows_for(&self, files: &[PullFile], comments: &[ReviewComment]) -> Vec<DiffRow> {
        let composing = self.composing.clone();
        // The comments and the composer hang under the line they
        // are about, so a line's row is followed by theirs.
        let after_line = |rows: &mut Vec<DiffRow>, path: &str, line: &diff::Line| {
            for comment in comments.iter().filter(|comment| {
                comment.path == path
                    && comment.line.is_some()
                    && match comment.side {
                        Side::Left => comment.line == line.old,
                        Side::Right => comment.line == line.new,
                    }
            }) {
                rows.push(DiffRow::Comment(comment.clone()));
            }
            if let Some((c_path, c_line, c_side)) = &composing
                && c_path == path
                && match c_side {
                    Side::Left => line.old == Some(*c_line),
                    Side::Right => line.new == Some(*c_line),
                }
            {
                rows.push(DiffRow::Composer);
            }
        };
        let mut rows = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let collapsed = self.collapsed.contains(&file.filename);
            rows.push(DiffRow::File {
                index,
                name: match &file.previous_filename {
                    Some(previous) => format!("{previous} → {}", file.filename).into(),
                    None => file.filename.clone().into(),
                },
                status: file.status,
                additions: file.additions,
                deletions: file.deletions,
                collapsed,
            });
            if collapsed {
                continue;
            }
            let Some(patch) = &file.patch else {
                rows.push(DiffRow::Note(
                    rust_i18n::t!("detail.file.no_diff").to_string().into(),
                ));
                continue;
            };
            let lines = diff::parse(patch);
            if self.split {
                for row in diff::split(&lines) {
                    match row {
                        diff::SplitRow::Hunk(text) => rows.push(DiffRow::Hunk(text.into())),
                        diff::SplitRow::Pair { left, right } => {
                            rows.push(DiffRow::Pair {
                                path: file.filename.clone(),
                                left: left.clone(),
                                right: right.clone(),
                            });
                            // A context line is on both sides; its
                            // comments hang once.
                            let context =
                                left.as_ref().is_some_and(|l| l.kind == diff::Kind::Context);
                            if let Some(l) = &left {
                                after_line(&mut rows, &file.filename, l);
                            }
                            if let Some(r) = &right
                                && !context
                            {
                                after_line(&mut rows, &file.filename, r);
                            }
                        }
                    }
                }
            } else {
                for line in lines {
                    rows.push(DiffRow::Line {
                        path: file.filename.clone(),
                        line: line.clone(),
                    });
                    after_line(&mut rows, &file.filename, &line);
                }
            }
            // Comments whose line has since changed hang at the
            // file's end, marked outdated.
            for comment in comments
                .iter()
                .filter(|comment| comment.path == file.filename && comment.line.is_none())
            {
                rows.push(DiffRow::Comment(comment.clone()));
            }
        }
        rows
    }

    /// Recompute the virtualized rows from what the store has, and ask for
    /// the pictures and the checks the conversation will draw.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.diff_rows.clear();
        self.lines.clear();
        if let Some(key) = self.item_key() {
            let (urls, sha) = {
                let store = self.store.read(cx);
                let detail = store.detail(&key).and_then(|fetch| fetch.value());
                let urls: Vec<String> = detail
                    .map(|detail| {
                        std::iter::once(detail.item.author.avatar_url.clone())
                            .chain(detail.item.assignees.iter().map(|u| u.avatar_url.clone()))
                            .chain(detail.comments.iter().map(|c| c.author.avatar_url.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let sha = detail
                    .and_then(|detail| detail.pull.as_ref())
                    .map(|pull| pull.head_sha.clone())
                    .filter(|sha| !sha.is_empty());
                (urls, sha)
            };
            let repo = key.0.clone();
            self.store.update(cx, |store, cx| {
                for url in urls {
                    store.ensure_avatar(&url, cx);
                }
                if let Some(sha) = sha {
                    store.ensure_checks(repo, sha, cx);
                }
            });
        }
        match &self.showing {
            Some(Showing::Item(key)) if self.tab == Tab::Files => {
                let (files, comments) = {
                    let store = self.store.read(cx);
                    (
                        store
                            .pull_files(key)
                            .and_then(|fetch| fetch.value())
                            .cloned()
                            .unwrap_or_default(),
                        store
                            .review_comments(key)
                            .and_then(|fetch| fetch.value())
                            .cloned()
                            .unwrap_or_default(),
                    )
                };
                self.diff_rows = self.diff_rows_for(&files, &comments);
                self.diff_state.reset(self.diff_rows.len());
            }
            Some(Showing::Commit { repo, sha }) => {
                let files = self
                    .store
                    .read(cx)
                    .commit(repo, sha)
                    .and_then(|fetch| fetch.value())
                    .map(|detail| detail.files.clone())
                    .unwrap_or_default();
                self.diff_rows = self.diff_rows_for(&files, &[]);
                self.diff_state.reset(self.diff_rows.len());
            }
            Some(Showing::File(key)) => {
                let path = key.1.clone();
                if let Some(text) = self
                    .store
                    .read(cx)
                    .content(key)
                    .and_then(|fetch| fetch.value())
                    .and_then(|content| content.text.as_deref())
                {
                    self.lines = text
                        .lines()
                        .map(|line| SharedString::from(line.to_string()))
                        .collect();
                    // Parsed once, here, rather than per frame: the rows
                    // ask the parse for their own line as they are drawn.
                    self.code = e1_ui::code::Code::parse(&path, text);
                }
            }
            Some(Showing::Log { repo, job, .. }) => {
                let store = self.store.read(cx);
                self.log_lines = store
                    .log(repo, *job)
                    .and_then(|fetch| fetch.value())
                    .map(|text| e1_ui::log::parse(text))
                    .unwrap_or_default();
                let job = store.job(repo, *job);
                self.log_steps = job
                    .and_then(|fetch| fetch.value())
                    .map(|job| job.steps.clone())
                    .unwrap_or_default();
                self.log_flat = job.is_some_and(|fetch| {
                    fetch.value().is_some_and(|job| job.steps.is_empty()) || fetch.error().is_some()
                });
                let starts: Vec<_> = self.log_steps.iter().map(|step| step.started_at).collect();
                self.log_owner = e1_ui::log::assign(&self.log_lines, &starts);
                if !self.log_touched {
                    self.log_open = self
                        .log_steps
                        .iter()
                        .filter(|step| {
                            matches!(step.state, CheckState::Failure | CheckState::Pending)
                        })
                        .map(|step| step.number)
                        .collect();
                }
                self.rebuild_log_rows();
                if self.ask_when_ready && !self.log_lines.is_empty() {
                    self.ask_when_ready = false;
                    let last = self.log_lines.len().saturating_sub(1);
                    self.log_selection = Some((10.min(last), 12.min(last)));
                    self.ask_soon = true;
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// A single muted line in the middle of the column.
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

    /// One comment: who, when, and what.
    fn comment(&self, comment: &Comment, cx: &App) -> AnyElement {
        let tokens = Tokens::global(cx);
        let picture = avatar(
            self.store.read(cx).avatar(&comment.author.avatar_url),
            &comment.author.login,
            px(20.),
            cx,
        );
        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(picture)
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_medium()
                            .text_color(tokens.colors().text_primary)
                            .child(comment.author.login.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().text_muted)
                            .child(age(Utc::now(), comment.created_at)),
                    ),
            )
            .child(
                div()
                    .pl_7()
                    .text_size(px(14.))
                    .line_height(relative(1.6))
                    .child(
                        TextView::markdown(("comment", comment.id as usize), comment.body.clone())
                            .selectable(true),
                    ),
            )
            .into_any_element()
    }

    /// The two chips that switch a diff between unified and split. A pull's
    /// files and a commit both carry them.
    fn diff_modes(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let chip = |this: &Self, split: bool, label: String, cx: &mut Context<Self>| {
            let selected = this.split == split;
            div()
                .id(if split { "mode-split" } else { "mode-unified" })
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
                .on_click(cx.listener(move |this, _, _, cx| this.set_split(split, cx)))
        };
        h_flex()
            .gap_0p5()
            .p_0p5()
            .rounded(px(tokens.radius.row))
            .bg(tokens.colors().bg_surface)
            .child(chip(
                self,
                false,
                rust_i18n::t!("diff.unified").to_string(),
                cx,
            ))
            .child(chip(
                self,
                true,
                rust_i18n::t!("diff.split").to_string(),
                cx,
            ))
            .into_any_element()
    }

    /// The two chips that switch a pull between its halves, and — on the
    /// files — the two that switch the diff between unified and split.
    fn tabs(&self, files: u64, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let mode = (self.tab == Tab::Files).then(|| self.diff_modes(cx));
        let chip = |this: &Self, tab: Tab, label: String, cx: &mut Context<Self>| {
            let selected = this.tab == tab;
            div()
                .id(match tab {
                    Tab::Conversation => "tab-conversation",
                    Tab::Files => "tab-files",
                })
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
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
        };
        let tabs = h_flex()
            .gap_0p5()
            .p_0p5()
            .rounded(px(tokens.radius.row))
            .bg(tokens.colors().bg_surface)
            .child(chip(
                self,
                Tab::Conversation,
                rust_i18n::t!("detail.tab.conversation").to_string(),
                cx,
            ))
            .child(chip(
                self,
                Tab::Files,
                format!("{} {files}", rust_i18n::t!("detail.tab.files")),
                cx,
            ))
            .into_any_element();
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .child(tabs)
            .children(mode)
            .into_any_element()
    }

    /// One row of the diff list.
    fn diff_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let Some(row) = self.diff_rows.get(index) else {
            return div().h(DIFF_ROW).into_any_element();
        };
        match row {
            DiffRow::File {
                index: file_index,
                name,
                status,
                additions,
                deletions,
                collapsed,
            } => {
                let mark_color = match status {
                    FileStatus::Added => tokens.colors().status_done,
                    FileStatus::Removed => tokens.colors().status_error,
                    FileStatus::Renamed => tokens.colors().accent,
                    FileStatus::Modified | FileStatus::Other => tokens.colors().status_attention,
                };
                let toggle = name.to_string();
                let file_index = *file_index;
                h_flex()
                    .id(("file", file_index))
                    .h(DIFF_ROW)
                    .w_full()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .bg(tokens.colors().bg_raised)
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().surface_hover()))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_file(toggle.clone(), cx)),
                    )
                    .child(
                        Icon::new(if *collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size_3()
                        .text_color(tokens.colors().text_muted),
                    )
                    .child(
                        div()
                            .w_3()
                            .text_size(px(11.5))
                            .font_family(mono.clone())
                            .text_color(mark_color)
                            .child(status.letter()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_size(px(11.5))
                            .font_family(mono)
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().status_done)
                            .child(format!("+{additions}")),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().status_error)
                            .child(format!("−{deletions}")),
                    )
                    .into_any_element()
            }
            DiffRow::Line { path, line } => {
                let in_ask = self.diff_picked(path, index);
                let ask_path = path.clone();
                let path = path.clone();
                let (fill, color, marker) = self.line_look(line, &tokens);
                let side = if line.new.is_some() {
                    Side::Right
                } else {
                    Side::Left
                };
                let number = line.new.or(line.old);
                let clickable = line.kind != diff::Kind::Hunk && number.is_some();
                let in_range = self.in_compose_range(&path, line);
                // The row grows with its line: a long line wraps rather
                // than running off the column.
                h_flex()
                    .id(("line", index))
                    .min_h(DIFF_ROW)
                    .w_full()
                    .px_1()
                    .items_start()
                    .font_family(mono)
                    .text_size(px(11.5))
                    .when_some(fill, |this, fill| this.bg(fill))
                    .when(in_range, |this| {
                        this.bg(tokens.colors().accent.opacity(0.18))
                    })
                    .when(in_ask, |this| this.bg(tokens.colors().accent.opacity(0.18)))
                    .when(clickable, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                // ⌥ picks the line out for an agent; a plain
                                // click means what it always meant, which is
                                // "comment here".
                                if event.modifiers().alt {
                                    this.pick_diff_row(
                                        ask_path.clone(),
                                        index,
                                        event.modifiers().shift,
                                        cx,
                                    );
                                } else if let Some(number) = number {
                                    let extend = event.modifiers().shift;
                                    this.start_line_comment(path.clone(), number, side, extend, cx);
                                }
                            }))
                    })
                    .child(self.gutter(line.old, &tokens))
                    .child(self.gutter(line.new, &tokens))
                    .child(div().w_3().flex_shrink_0().text_color(color).child(marker))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(color)
                            .child(line.text.clone()),
                    )
                    .into_any_element()
            }
            DiffRow::Pair { path, left, right } => {
                let half = |this: &Self,
                            line: &Option<diff::Line>,
                            side: Side,
                            cx: &mut Context<Self>|
                 -> AnyElement {
                    let Some(line) = line else {
                        return div()
                            .flex_1()
                            .min_w_0()
                            .bg(tokens.colors().bg_surface.opacity(0.4))
                            .into_any_element();
                    };
                    let (fill, color, _) = this.line_look(line, &tokens);
                    let number = match side {
                        Side::Left => line.old,
                        Side::Right => line.new,
                    };
                    let in_range = matches!(&this.composing, Some((_, _, open)) if *open == side)
                        && this.in_compose_range(path, line);
                    let path = path.clone();
                    // Each half wraps its own line; the row is as tall as
                    // the taller half.
                    h_flex()
                        .id((if side == Side::Left { "left" } else { "right" }, index))
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .when_some(fill, |this, fill| this.bg(fill))
                        .when(in_range, |this| {
                            this.bg(tokens.colors().accent.opacity(0.18))
                        })
                        .when(number.is_some(), |this| {
                            this.cursor_pointer()
                                .hover(|this| this.bg(tokens.colors().row_hover()))
                                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                    if let Some(number) = number {
                                        let extend = event.modifiers().shift;
                                        this.start_line_comment(
                                            path.clone(),
                                            number,
                                            side,
                                            extend,
                                            cx,
                                        );
                                    }
                                }))
                        })
                        .child(this.gutter(number, &tokens))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_1()
                                .text_color(color)
                                .child(line.text.clone()),
                        )
                        .into_any_element()
                };
                let left = half(self, left, Side::Left, cx);
                let right = half(self, right, Side::Right, cx);
                h_flex()
                    .min_h(DIFF_ROW)
                    .w_full()
                    .items_stretch()
                    .font_family(mono)
                    .text_size(px(11.5))
                    .child(left)
                    .child(
                        div()
                            .w_px()
                            .flex_shrink_0()
                            .bg(tokens.colors().border_subtle),
                    )
                    .child(right)
                    .into_any_element()
            }
            DiffRow::Hunk(text) => div()
                .h(DIFF_ROW)
                .w_full()
                .px_3()
                .flex()
                .items_center()
                .font_family(mono)
                .text_size(px(11.5))
                .bg(tokens.colors().code_bg)
                .text_color(tokens.colors().text_muted)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(text.clone())
                .into_any_element(),
            DiffRow::Comment(comment) => {
                let picture = avatar(
                    self.store.read(cx).avatar(&comment.author.avatar_url),
                    &comment.author.login,
                    px(18.),
                    cx,
                );
                let outdated = comment.line.is_none();
                let range = match (comment.start_line, comment.line) {
                    (Some(start), Some(end)) => Some(
                        rust_i18n::t!("diff.comment.range", start = start, end = end).to_string(),
                    ),
                    _ => None,
                };
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .child(
                        v_flex()
                            .w_full()
                            .gap_1p5()
                            .p_3()
                            .rounded(px(tokens.radius.panel))
                            .bg(tokens.colors().bg_surface)
                            .border_l_2()
                            .border_color(tokens.colors().accent.opacity(0.6))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(picture)
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_medium()
                                            .text_color(tokens.colors().text_primary)
                                            .child(comment.author.login.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(tokens.colors().text_muted)
                                            .child(age(Utc::now(), comment.created_at)),
                                    )
                                    .children(range.map(|range| {
                                        div()
                                            .text_size(px(11.))
                                            .text_color(tokens.colors().text_muted)
                                            .child(range)
                                    }))
                                    .when(outdated, |this| {
                                        this.child(
                                            div()
                                                .px_1p5()
                                                .rounded(px(tokens.radius.control()))
                                                .bg(tokens.colors().status_attention.opacity(0.2))
                                                .text_size(px(10.5))
                                                .text_color(tokens.colors().status_attention)
                                                .child(rust_i18n::t!("diff.outdated").to_string()),
                                        )
                                    }),
                            )
                            .child(
                                div().text_size(px(13.)).line_height(relative(1.5)).child(
                                    TextView::markdown(
                                        ("review-comment", comment.id as usize),
                                        comment.body.clone(),
                                    )
                                    .selectable(true),
                                ),
                            ),
                    )
                    .into_any_element()
            }
            DiffRow::Composer => div()
                .w_full()
                .px_3()
                .py_2()
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .p_1()
                        .rounded(px(tokens.radius.control() + 2.))
                        .bg(tokens.colors().bg_surface)
                        .border_1()
                        .border_color(tokens.colors().border_strong)
                        .child(
                            div()
                                .px_1()
                                .pt_0p5()
                                .text_size(px(11.))
                                .text_color(tokens.colors().text_muted)
                                .child(match (self.compose_start, &self.composing) {
                                    (Some(start), Some((_, end, _))) => rust_i18n::t!(
                                        "diff.comment.range",
                                        start = start,
                                        end = end
                                    )
                                    .to_string(),
                                    (_, Some((_, line, _))) => {
                                        rust_i18n::t!("diff.comment.line", line = line).to_string()
                                    }
                                    _ => String::new(),
                                }),
                        )
                        .child(Textarea::new(&self.review_input))
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_1p5()
                                .pr_1()
                                .pb_0p5()
                                .child(self.button(
                                    "line-comment-cancel",
                                    rust_i18n::t!("detail.merge.cancel").to_string(),
                                    false,
                                    cx,
                                    |this, cx| this.cancel_line_comment(cx),
                                ))
                                .child(self.button(
                                    "line-comment-send",
                                    rust_i18n::t!("diff.comment.send").to_string(),
                                    true,
                                    cx,
                                    |this, cx| this.send_line_comment(cx),
                                )),
                        ),
                )
                .into_any_element(),
            DiffRow::Note(text) => div()
                .h(DIFF_ROW)
                .px_3()
                .text_size(px(11.5))
                .text_color(tokens.colors().text_muted)
                .child(text.clone())
                .into_any_element(),
        }
    }

    /// How a diff line is painted: its fill, its text colour, its marker.
    fn line_look(&self, line: &diff::Line, tokens: &Tokens) -> (Option<Hsla>, Hsla, &'static str) {
        match line.kind {
            diff::Kind::Added => (
                Some(tokens.colors().status_done.opacity(0.12)),
                tokens.colors().text_primary,
                "+",
            ),
            diff::Kind::Removed => (
                Some(tokens.colors().status_error.opacity(0.12)),
                tokens.colors().text_secondary,
                "-",
            ),
            diff::Kind::Hunk => (
                Some(tokens.colors().code_bg),
                tokens.colors().text_muted,
                "",
            ),
            diff::Kind::Context => (None, tokens.colors().text_secondary, " "),
        }
    }

    /// A line number in the gutter.
    fn gutter(&self, value: Option<u32>, tokens: &Tokens) -> AnyElement {
        div()
            .w(px(40.))
            .flex_shrink_0()
            .text_right()
            .pr_1()
            .text_color(tokens.colors().text_muted)
            .child(value.map(|v| v.to_string()).unwrap_or_default())
            .into_any_element()
    }

    /// One row of the log screen.
    fn log_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        match self.log_rows.get(index) {
            Some(LogRow::Step(step)) => self.log_step_row(*step, mono, cx),
            Some(LogRow::Line(line)) => self.log_line_row(*line, mono, cx),
            Some(LogRow::NoSteps) => div()
                .w_full()
                .px_4()
                .py_2()
                .text_size(px(11.5))
                .text_color(Tokens::global(cx).colors().text_muted)
                .child(rust_i18n::t!("log.no_steps").to_string())
                .into_any_element(),
            None => div().h(CODE_ROW).into_any_element(),
        }
    }

    /// A step's heading, as GitHub draws it: a chevron to fold it, how it
    /// went, its name, and how long it took.
    fn log_step_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let Some(step) = self.log_steps.get(index) else {
            return div().h(CODE_ROW).into_any_element();
        };
        let colors = tokens.colors();
        let open = self.log_open.contains(&step.number);
        let failed = step.state == CheckState::Failure;
        let mark: AnyElement = match step.state {
            CheckState::Success => Icon::new(IconName::Check)
                .size_3p5()
                .text_color(colors.status_done)
                .into_any_element(),
            CheckState::Failure => Icon::new(IconName::Close)
                .size_3p5()
                .text_color(colors.status_error)
                .into_any_element(),
            CheckState::Pending => spinner(px(14.), colors.status_attention),
            // Skipped: a ring with a dash through it.
            CheckState::Neutral => div()
                .size_3p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(colors.text_muted)
                .child(
                    Icon::new(IconName::Minus)
                        .size_2p5()
                        .text_color(colors.text_muted),
                )
                .into_any_element(),
        };
        let number = step.number;
        let hover = colors.row_hover();
        h_flex()
            .id(("log-step", index))
            .w_full()
            .pl_3()
            .pr_4()
            .py_1p5()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .border_t_1()
            .border_color(colors.border_subtle)
            .when(failed, |this| this.bg(colors.status_error.opacity(0.08)))
            .hover(move |this| this.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_step(number, cx)))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3p5()
                .text_color(colors.text_muted),
            )
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(if failed {
                        colors.status_error
                    } else {
                        colors.text_primary
                    })
                    .child(step.name.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(mono)
                    .text_size(px(11.))
                    .text_color(colors.text_muted)
                    .child(step.duration()),
            )
            .into_any_element()
    }

    /// One line of a job's log: the clock in the gutter, the text coloured
    /// by what the runner marked it as, wrapping when it is long.
    fn log_line_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        use e1_ui::log::Kind;
        let tokens = Tokens::global(cx);
        let Some(line) = self.log_lines.get(index) else {
            return div().h(CODE_ROW).into_any_element();
        };
        let (color, fill, weight) = match line.kind {
            Kind::Plain => (tokens.colors().text_secondary, None, false),
            Kind::Group => (
                tokens.colors().text_primary,
                Some(tokens.colors().bg_raised),
                true,
            ),
            Kind::EndGroup => (tokens.colors().text_muted, None, false),
            Kind::Command => (tokens.colors().accent, None, false),
            Kind::Error => (
                tokens.colors().status_error,
                Some(tokens.colors().status_error.opacity(0.12)),
                false,
            ),
            Kind::Warning => (
                tokens.colors().status_attention,
                Some(tokens.colors().status_attention.opacity(0.12)),
                false,
            ),
            Kind::Notice => (tokens.colors().text_muted, None, false),
        };
        // Under a step the lines sit in from its heading; without steps
        // they run from the edge.
        let inset = if self.log_steps.is_empty() { 8. } else { 36. };
        let picked = self.log_picked(index);
        h_flex()
            .id(("log-line", index))
            .w_full()
            .min_h(CODE_ROW)
            .pl(px(inset))
            .pr_3()
            .py(px(1.))
            .items_start()
            .font_family(mono)
            .text_size(px(11.5))
            .when_some(fill, |this, fill| this.bg(fill))
            // Picking lines is what the ask box asks about: a click starts
            // the pick and a ⇧-click stretches it, as it does on the diff.
            .when(picked, |this| this.bg(tokens.colors().accent.opacity(0.18)))
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.pick_log_line(index, event.modifiers().shift, cx)
            }))
            .child(
                div()
                    .w(px(64.))
                    .flex_shrink_0()
                    .pr_3()
                    .whitespace_nowrap()
                    .text_color(tokens.colors().text_muted)
                    .child(line.time.clone().unwrap_or_default()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(color)
                    .when(weight, |this| this.font_medium())
                    .child(line.text.clone()),
            )
            .into_any_element()
    }

    /// One line of a file.
    fn code_row(&self, index: usize, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx);
        let Some(line) = self.lines.get(index) else {
            return div().h(CODE_ROW).into_any_element();
        };
        // The parse holds the whole file; a row asks it only about itself,
        // and a file with no grammar draws in the plain colour.
        let styles = self
            .code
            .as_ref()
            .map(|code| code.line(index, &e1_ui::code::theme(cx)))
            .unwrap_or_default();
        let picked = self.code_picked(index);
        h_flex()
            .id(("code-line", index))
            .h(CODE_ROW)
            .w_full()
            .px_1()
            .items_center()
            .font_family(mono)
            .text_size(px(11.5))
            .whitespace_nowrap()
            .overflow_hidden()
            .when(picked, |this| this.bg(tokens.colors().accent.opacity(0.18)))
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.pick_code_line(index, event.modifiers().shift, cx)
            }))
            .child(
                div()
                    .w(px(48.))
                    .flex_shrink_0()
                    .text_right()
                    .pr_3()
                    .text_color(tokens.colors().text_muted)
                    .child((index + 1).to_string()),
            )
            .child(
                div()
                    .text_color(tokens.colors().text_primary)
                    .child(StyledText::new(line.clone()).with_highlights(styles)),
            )
            .into_any_element()
    }

    /// The files tab's body: the virtualized diff, or why there is none.
    fn diff_list(&self, key: &ItemKey, mono: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let fetch = self.store.read(cx).pull_files(key).cloned();
        let (files, error, loading) = match &fetch {
            Some(fetch) => (
                fetch.value().cloned(),
                fetch.error().map(str::to_string),
                fetch.is_loading(),
            ),
            None => (None, None, false),
        };
        match files {
            Some(files) if files.is_empty() => {
                self.notice(rust_i18n::t!("detail.files.empty").to_string(), false, cx)
            }
            Some(_) => {
                let this = cx.entity();
                list(self.diff_state.clone(), move |index, _window, cx| {
                    this.update(cx, |this, cx| this.diff_row(index, mono.clone(), cx))
                })
                .flex_1()
                .size_full()
                .into_any_element()
            }
            None => match error {
                Some(error) => self.notice(error, true, cx),
                None if loading => crate::skeleton::diff(cx),
                None => self.notice(rust_i18n::t!("detail.files.empty").to_string(), false, cx),
            },
        }
    }

    /// A small button: filled for the one thing the row is for, quiet
    /// otherwise, and `danger` when it is the destructive one.
    fn button(
        &self,
        id: impl Into<ElementId>,
        label: String,
        loud: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        div()
            .id(id)
            .px_2p5()
            .py_1()
            .rounded(px(tokens.radius.control()))
            .cursor_pointer()
            .text_size(px(11.5))
            .when(loud, |this| {
                this.bg(tokens.colors().accent)
                    .text_color(tokens.colors().bg_window)
                    .hover(|this| this.opacity(0.85))
            })
            .when(!loud, |this| {
                this.bg(tokens.colors().bg_surface)
                    .text_color(tokens.colors().text_primary)
                    .hover(|this| this.bg(tokens.colors().row_hover()))
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .into_any_element()
    }

    /// Close or reopen, open on the web, and what the last write said.
    fn actions(
        &self,
        key: &ItemKey,
        detail: &crate::store::Detail,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let action = self.store.read(cx).action(key).cloned();
        let busy = action.as_ref().is_some_and(|action| action.is_loading());
        let complaint = action
            .as_ref()
            .and_then(|action| action.error().map(str::to_string));
        let item = &detail.item;
        let open = item.status == e1_github::Status::Open;
        let merged = matches!(item.kind, e1_github::Kind::Pull { merged: true, .. });
        let url = item.html_url.clone();

        let mut row = h_flex().gap_2().items_center().flex_wrap();
        if busy {
            row = row.child(
                div()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("detail.working").to_string()),
            );
        } else if !merged {
            let (label, to_open) = if open {
                (rust_i18n::t!("detail.close").to_string(), false)
            } else {
                (rust_i18n::t!("detail.reopen").to_string(), true)
            };
            row = row.child(
                self.button("toggle-open", label, false, cx, move |this, cx| {
                    this.set_open(to_open, cx)
                }),
            );
        }
        row = row.child(
            h_flex()
                .id("open-web")
                .gap_1()
                .items_center()
                .px_2p5()
                .py_1()
                .rounded(px(tokens.radius.control()))
                .cursor_pointer()
                .text_size(px(11.5))
                .text_color(tokens.colors().text_secondary)
                .bg(tokens.colors().bg_surface)
                .hover(|this| this.bg(tokens.colors().row_hover()))
                .child(
                    Icon::new(IconName::ExternalLink)
                        .size_3()
                        .text_color(tokens.colors().text_secondary),
                )
                .child(rust_i18n::t!("detail.open_on_github").to_string())
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
        );
        // Asking about the whole item, for when nothing has been picked
        // out of it. Only when there is an agent to ask.
        if let Some(agent) = self.store.read(cx).chosen_agent().map(|agent| agent.kind) {
            row = row.child(
                h_flex()
                    .id("ask-item")
                    .gap_1()
                    .items_center()
                    .px_2p5()
                    .py_1()
                    .rounded(px(tokens.radius.control()))
                    .cursor_pointer()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().accent)
                    .bg(tokens.colors().accent.opacity(0.14))
                    .hover(|this| this.bg(tokens.colors().accent.opacity(0.22)))
                    .child(Icon::new(IconName::Bot).size_3())
                    .child(rust_i18n::t!("ask.to", agent = agent.label()).to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.open_ask(window, cx))),
            );
        }
        if let Some(complaint) = complaint {
            row = row.child(
                div()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().status_error)
                    .child(complaint),
            );
        }
        row.into_any_element()
    }

    /// A facet's row: its name, what the item has, and the gear that opens
    /// its picker under the row.
    fn facet_row(
        &self,
        picker: Picker,
        name: String,
        values: AnyElement,
        popover: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        let open = self.picker == Some(picker);
        let id = match picker {
            Picker::Labels => "facet-labels",
            Picker::Assignees => "facet-assignees",
            Picker::Projects => "facet-projects",
        };
        h_flex()
            .w_full()
            .relative()
            .py_1()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w(px(72.))
                    .flex_shrink_0()
                    .text_size(px(11.5))
                    .font_medium()
                    .text_color(if open {
                        tokens.colors().accent
                    } else {
                        tokens.colors().text_secondary
                    })
                    .child(name),
            )
            .child(div().flex_1().overflow_hidden().child(values))
            .child(
                div()
                    .id(id)
                    .p_1()
                    .rounded(px(tokens.radius.control()))
                    .cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
                    .child(
                        Icon::new(IconName::Settings)
                            .size_3p5()
                            .text_color(if open {
                                tokens.colors().accent
                            } else {
                                tokens.colors().text_muted
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_picker(picker, cx))),
            )
            .children(popover)
            .into_any_element()
    }

    /// One row of a picker: something to add or remove, with a check when
    /// the item has it. Eight arguments because a row is eight facts; a
    /// struct for them would be the same eight facts with a name.
    #[allow(clippy::too_many_arguments)]
    fn picker_row(
        &self,
        id: impl Into<ElementId>,
        leading: AnyElement,
        title: String,
        subtitle: Option<String>,
        has: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        h_flex()
            .id(id)
            .w_full()
            .px_2p5()
            .py_1p5()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .child(div().w_4().flex_shrink_0().child(if has {
                Icon::new(IconName::Check)
                    .size_3p5()
                    .text_color(tokens.colors().text_primary)
                    .into_any_element()
            } else {
                div().into_any_element()
            }))
            .child(leading)
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(title),
                    )
                    .children(subtitle.map(|subtitle| {
                        div()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(subtitle)
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .into_any_element()
    }

    /// The picker under a facet: a filter, then the rows that match it.
    ///
    /// Floated over the page with `deferred` and `anchored`, so opening it
    /// does not push the conversation down, and closed by a press anywhere
    /// outside it — a popover, the way GitHub's is, in place of a menu.
    fn picker(
        &self,
        placeholder: String,
        rows: Vec<AnyElement>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        let _ = placeholder;
        let card = v_flex()
            .id("picker")
            .w(px(360.))
            .rounded(px(tokens.radius.panel))
            .bg(tokens.colors().popover())
            .border_1()
            .border_color(tokens.colors().border_strong)
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.picker = None;
                cx.notify();
            }))
            .child(div().p_2().child(Input::new(&self.filter).cleanable(true)))
            .child(match error {
                Some(error) => div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().status_error)
                    .child(error)
                    .into_any_element(),
                None if rows.is_empty() => div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("facet.no_match").to_string())
                    .into_any_element(),
                None => v_flex()
                    .id("picker-rows")
                    .w_full()
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element(),
            });
        div()
            .absolute()
            .top(px(26.))
            .left_0()
            .child(
                deferred(
                    anchored()
                        .position_mode(AnchoredPositionMode::Local)
                        .snap_to_window_with_margin(px(8.))
                        .child(card),
                )
                .with_priority(1),
            )
            .into_any_element()
    }

    /// The labels, the assignees and the projects, each a heading, its
    /// values, and — while its gear is on — its picker.
    fn facets(
        &self,
        key: &ItemKey,
        detail: &crate::store::Detail,
        labels: &[LabelChip],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let muted = tokens.colors().text_muted;
        let item = detail.item.clone();
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let matches = |text: &str, more: Option<&str>| {
            query.is_empty()
                || text.to_lowercase().contains(&query)
                || more.is_some_and(|more| more.to_lowercase().contains(&query))
        };
        // Copied out of the store, so the rows below can bind listeners
        // through `cx` without a read of the store held across them.
        let (
            viewer,
            avatars,
            memberships,
            offered_labels,
            offered_people,
            offered_projects,
            membership_error,
        ) = {
            let store = self.store.read(cx);
            let avatars: Vec<Option<std::path::PathBuf>> = item
                .assignees
                .iter()
                .map(|user| store.avatar(&user.avatar_url))
                .collect();
            (
                store.viewer().value().map(|viewer| viewer.login.clone()),
                avatars,
                store
                    .memberships(key)
                    .and_then(|fetch| fetch.value())
                    .cloned()
                    .unwrap_or_default(),
                store.repo_labels(&key.0).cloned(),
                store.candidates(&key.0).cloned(),
                store.projects(&key.0.owner).cloned(),
                store
                    .memberships(key)
                    .and_then(|fetch| fetch.error().map(str::to_string)),
            )
        };
        let none = || {
            div()
                .text_size(px(11.5))
                .text_color(muted)
                .child(rust_i18n::t!("detail.none").to_string())
                .into_any_element()
        };

        // Labels.
        let label_values: AnyElement = if labels.is_empty() {
            none()
        } else {
            h_flex()
                .gap_1p5()
                .flex_wrap()
                .children(labels.iter().map(|label| {
                    let (fill, text) = label.paint(tokens.appearance);
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(fill)
                        .text_size(px(11.5))
                        .text_color(text)
                        .child(label.name.clone())
                }))
                .into_any_element()
        };
        let label_picker = (self.picker == Some(Picker::Labels)).then(|| {
            let rows: Vec<AnyElement> = offered_labels
                .as_ref()
                .and_then(|fetch| fetch.value())
                .map(|offered| {
                    offered
                        .iter()
                        .filter(|label| matches(&label.name, label.description.as_deref()))
                        .enumerate()
                        .map(|(index, label)| {
                            let has = item.labels.iter().any(|mine| mine.name == label.name);
                            let name = label.name.clone();
                            let color = e1_ui::theme::parse_hex(&label.color).unwrap_or(muted);
                            self.picker_row(
                                ("pick-label", index),
                                div()
                                    .size_3()
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(color)
                                    .into_any_element(),
                                label.name.clone(),
                                label.description.clone(),
                                has,
                                cx,
                                move |this, cx| this.toggle_label(name.clone(), has, cx),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.picker(
                rust_i18n::t!("facet.filter.labels").to_string(),
                rows,
                offered_labels
                    .as_ref()
                    .and_then(|f| f.error().map(str::to_string)),
                cx,
            )
        });

        // Assignees.
        let mut assignee_values = h_flex().gap_2().flex_wrap().items_center();
        if item.assignees.is_empty() {
            assignee_values = assignee_values.child(none());
            if let Some(me) = viewer.clone() {
                let already = item.assignees.iter().any(|user| user.login == me);
                if !already {
                    let login = me.clone();
                    assignee_values = assignee_values.child(
                        div()
                            .id("assign-self")
                            .text_size(px(11.5))
                            .text_color(tokens.colors().accent)
                            .cursor_pointer()
                            .child(rust_i18n::t!("facet.assign_self").to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_assignee(login.clone(), false, cx)
                            })),
                    );
                }
            }
        }
        for (user, picture) in item.assignees.iter().zip(avatars) {
            assignee_values = assignee_values.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(avatar(picture, &user.login, px(16.), cx))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().text_secondary)
                            .child(user.login.clone()),
                    ),
            );
        }
        let assignee_picker = (self.picker == Some(Picker::Assignees)).then(|| {
            let rows: Vec<AnyElement> = offered_people
                .as_ref()
                .and_then(|fetch| fetch.value())
                .map(|people| {
                    people
                        .iter()
                        .filter(|user| matches(&user.login, None))
                        .enumerate()
                        .map(|(index, user)| {
                            let has = item.assignees.iter().any(|mine| mine.login == user.login);
                            let login = user.login.clone();
                            let picture = self.store.read(cx).avatar(&user.avatar_url);
                            self.picker_row(
                                ("pick-assignee", index),
                                avatar(picture, &user.login, px(18.), cx),
                                user.login.clone(),
                                None,
                                has,
                                cx,
                                move |this, cx| this.toggle_assignee(login.clone(), has, cx),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.picker(
                rust_i18n::t!("facet.filter.assignees").to_string(),
                rows,
                offered_people
                    .as_ref()
                    .and_then(|f| f.error().map(str::to_string)),
                cx,
            )
        });

        // Projects.
        let project_values: AnyElement = if memberships.is_empty() {
            none()
        } else {
            h_flex()
                .gap_1p5()
                .flex_wrap()
                .children(memberships.iter().map(|membership| {
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded(px(tokens.radius.control()))
                        .bg(tokens.colors().code_bg)
                        .text_size(px(11.5))
                        .text_color(tokens.colors().text_secondary)
                        .child(membership.title.clone())
                }))
                .into_any_element()
        };
        let project_picker = (self.picker == Some(Picker::Projects)).then(|| {
            let rows: Vec<AnyElement> = offered_projects
                .as_ref()
                .and_then(|fetch| fetch.value())
                .map(|projects| {
                    projects
                        .iter()
                        .filter(|project| !project.closed && matches(&project.title, None))
                        .enumerate()
                        .map(|(index, project)| {
                            let item_id = memberships
                                .iter()
                                .find(|m| m.project_id == project.id)
                                .map(|m| m.item_id.clone());
                            let has = item_id.is_some();
                            let project_id = project.id.clone();
                            self.picker_row(
                                ("pick-project", index),
                                Icon::new(IconName::LayoutDashboard)
                                    .size_3p5()
                                    .text_color(muted)
                                    .into_any_element(),
                                project.title.clone(),
                                Some(format!("#{}", project.number)),
                                has,
                                cx,
                                move |this, cx| {
                                    this.toggle_project(project_id.clone(), item_id.clone(), cx)
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let error = offered_projects
                .as_ref()
                .and_then(|f| f.error().map(str::to_string))
                .or(membership_error);
            self.picker(
                rust_i18n::t!("facet.filter.projects").to_string(),
                rows,
                error,
                cx,
            )
        });

        v_flex()
            .w_full()
            .child(self.facet_row(
                Picker::Labels,
                rust_i18n::t!("detail.labels").to_string(),
                label_values,
                label_picker,
                cx,
            ))
            .child(self.facet_row(
                Picker::Assignees,
                rust_i18n::t!("detail.assignees").to_string(),
                assignee_values.into_any_element(),
                assignee_picker,
                cx,
            ))
            .child(self.facet_row(
                Picker::Projects,
                rust_i18n::t!("detail.projects").to_string(),
                project_values,
                project_picker,
                cx,
            ))
            .into_any_element()
    }

    /// What the checks came to, whether the branch conflicts, and the
    /// merge — GitHub's own card, rebuilt.
    fn merge_card(
        &self,
        key: &ItemKey,
        detail: &crate::store::Detail,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let pull = detail.pull.as_ref()?;
        let item = &detail.item;
        let open = item.status == e1_github::Status::Open;
        let draft = matches!(item.kind, e1_github::Kind::Pull { draft: true, .. });
        if pull.head_sha.is_empty() {
            return None;
        }
        let checks_fetch = self.store.read(cx).checks(&key.0, &pull.head_sha).cloned();
        let checks = checks_fetch
            .as_ref()
            .and_then(|fetch| fetch.value())
            .cloned();
        let checks_error = checks_fetch
            .as_ref()
            .and_then(|fetch| fetch.error().map(str::to_string));
        let busy = self
            .store
            .read(cx)
            .action(key)
            .is_some_and(|action| action.is_loading());
        let green = tokens.colors().status_done;
        let red = tokens.colors().status_error;
        let amber = tokens.colors().status_attention;
        let muted = tokens.colors().text_muted;

        let status_row =
            |icon: AnyElement, title: String, subtitle: String, trailing: Option<AnyElement>| {
                h_flex()
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .items_center()
                    .child(icon)
                    .child(
                        v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_medium()
                                    .text_color(tokens.colors().text_primary)
                                    .child(title),
                            )
                            .child(div().text_size(px(11.5)).text_color(muted).child(subtitle)),
                    )
                    .children(trailing)
            };
        let ring = |color: Hsla, mark: AnyElement| {
            div()
                .size_6()
                .flex_shrink_0()
                .rounded_full()
                .bg(color)
                .flex()
                .items_center()
                .justify_center()
                .child(mark)
                .into_any_element()
        };
        let badge = |color: Hsla, icon: IconName| {
            ring(
                color,
                Icon::new(icon)
                    .size_3p5()
                    .text_color(tokens.colors().bg_window)
                    .into_any_element(),
            )
        };
        // Still running: the badge turns.
        let turning = |color: Hsla| ring(color, spinner(px(14.), tokens.colors().bg_window));

        // Checks.
        let (passed, failed, pending) = checks.as_ref().map(|c| c.tally()).unwrap_or_default();
        let overall = checks.as_ref().map(|c| c.overall());
        let (check_icon, check_title, check_sub) = match overall {
            None if checks_error.is_some() => (
                badge(red, IconName::Close),
                rust_i18n::t!("checks.unknown").to_string(),
                checks_error.clone().unwrap_or_default(),
            ),
            None => (
                turning(muted),
                rust_i18n::t!("checks.unknown").to_string(),
                String::new(),
            ),
            Some(CheckState::Neutral) => (
                badge(muted, IconName::Minus),
                rust_i18n::t!("checks.none").to_string(),
                String::new(),
            ),
            Some(CheckState::Success) => (
                badge(green, IconName::Check),
                rust_i18n::t!("checks.passed").to_string(),
                rust_i18n::t!("checks.passed_count", count = passed).to_string(),
            ),
            Some(CheckState::Failure) => (
                badge(red, IconName::Close),
                rust_i18n::t!("checks.failed").to_string(),
                rust_i18n::t!("checks.failed_count", failed = failed, passed = passed).to_string(),
            ),
            Some(CheckState::Pending) => (
                turning(amber),
                rust_i18n::t!("checks.pending").to_string(),
                rust_i18n::t!("checks.pending_count", count = pending).to_string(),
            ),
        };
        let has_runs = checks.as_ref().is_some_and(|c| !c.runs.is_empty());
        let chevron = has_runs.then(|| {
            Icon::new(if self.checks_open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size_3p5()
            .text_color(muted)
            .into_any_element()
        });
        let checks_row = div()
            .id("checks-row")
            .w_full()
            .when(has_runs, |this| this.cursor_pointer())
            .child(status_row(check_icon, check_title, check_sub, chevron))
            .on_click(cx.listener(|this, _, _, cx| {
                this.checks_open = !this.checks_open;
                cx.notify();
            }));
        let runs: Vec<AnyElement> = if self.checks_open {
            checks
                .as_ref()
                .map(|checks| {
                    checks
                        .runs
                        .iter()
                        .enumerate()
                        .map(|(index, run)| {
                            let mark = |color: Hsla, icon: IconName| {
                                Icon::new(icon)
                                    .size_3p5()
                                    .text_color(color)
                                    .into_any_element()
                            };
                            let mark = match run.state {
                                CheckState::Success => mark(green, IconName::Check),
                                CheckState::Failure => mark(red, IconName::Close),
                                CheckState::Pending => spinner(px(14.), amber),
                                CheckState::Neutral => mark(muted, IconName::Minus),
                            };
                            let url = run.html_url.clone();
                            // Two plain buttons, each doing one thing: the
                            // row itself does nothing on a click, so that
                            // opening the browser cannot also open the log.
                            let chip = |id: (&'static str, usize),
                                        icon: IconName,
                                        label: String,
                                        color: Hsla| {
                                h_flex()
                                    .id(id)
                                    .flex_shrink_0()
                                    .gap_1()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded(px(5.))
                                    .border_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .text_size(px(11.))
                                    .text_color(color)
                                    .cursor_pointer()
                                    .hover(|this| this.bg(tokens.colors().row_hover()))
                                    .child(Icon::new(icon).size_3().text_color(color))
                                    .child(label)
                            };
                            h_flex()
                                .id(("check-run", index))
                                .w_full()
                                .pl(px(52.))
                                .pr_4()
                                .py_1p5()
                                .gap_2()
                                .items_center()
                                .border_t_1()
                                .border_color(tokens.colors().border_subtle)
                                .child(mark)
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(11.5))
                                        .text_color(tokens.colors().text_secondary)
                                        .truncate()
                                        .child(run.name.clone()),
                                )
                                .children(run.actions.then(|| {
                                    let (repo, job, name) =
                                        (key.0.clone(), run.id, run.name.clone());
                                    chip(
                                        ("check-log", index),
                                        IconName::SquareTerminal,
                                        rust_i18n::t!("checks.log").to_string(),
                                        tokens.colors().accent,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.show_log(repo.clone(), job, name.clone(), cx)
                                        },
                                    ))
                                }))
                                .children(url.map(|url| {
                                    chip(
                                        ("check-details", index),
                                        IconName::ExternalLink,
                                        rust_i18n::t!("checks.details").to_string(),
                                        tokens.colors().text_muted,
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                                }))
                                .into_any_element()
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Conflicts.
        let conflicts_row = match pull.mergeable {
            Some(true) => status_row(
                badge(green, IconName::Check),
                rust_i18n::t!("conflicts.none").to_string(),
                rust_i18n::t!("conflicts.auto").to_string(),
                None,
            ),
            Some(false) => status_row(
                badge(red, IconName::Close),
                rust_i18n::t!("conflicts.some").to_string(),
                String::new(),
                None,
            ),
            None => status_row(
                turning(muted),
                rust_i18n::t!("conflicts.unknown").to_string(),
                String::new(),
                None,
            ),
        };

        // The merge.
        let can_merge = !draft && pull.mergeable != Some(false) && !busy;
        let method = self.merge_method;
        let button_label = rust_i18n::t!(match (self.confirm_merge, method) {
            (false, MergeMethod::Merge) => "merge.button.merge",
            (false, MergeMethod::Squash) => "merge.button.squash",
            (false, MergeMethod::Rebase) => "merge.button.rebase",
            (true, MergeMethod::Merge) => "merge.confirm.merge",
            (true, MergeMethod::Squash) => "merge.confirm.squash",
            (true, MergeMethod::Rebase) => "merge.confirm.rebase",
        })
        .to_string();
        let button_color = if can_merge {
            tokens.colors().merge_button()
        } else {
            muted
        };
        let corner = px(tokens.radius.control() + 2.);
        let merge_button = h_flex()
            .h(px(30.))
            .child(
                div()
                    .id("merge")
                    .h_full()
                    .flex()
                    .items_center()
                    .px_3()
                    .rounded_l(corner)
                    .bg(button_color)
                    .text_size(px(12.))
                    .font_medium()
                    .text_color(gpui::white())
                    .when(can_merge, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.opacity(0.9))
                            .on_click(cx.listener(|this, _, _, cx| this.press_merge(cx)))
                    })
                    .child(button_label),
            )
            .child(
                div()
                    .id("merge-menu")
                    .h_full()
                    .flex()
                    .items_center()
                    .px_2()
                    .rounded_r(corner)
                    .bg(button_color)
                    .border_l_1()
                    .border_color(gpui::white().opacity(0.25))
                    .when(can_merge, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.opacity(0.9))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.merge_menu = !this.merge_menu;
                                cx.notify();
                            }))
                    })
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3p5()
                            .text_color(gpui::white()),
                    ),
            );
        // A draft's button is the way out of being one; a ready pull can
        // go back, in the small print, the way GitHub offers it.
        let ready = draft.then(|| {
            div()
                .id("ready")
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .rounded(corner)
                .bg(tokens.colors().merge_button())
                .text_size(px(12.))
                .font_medium()
                .text_color(gpui::white())
                .cursor_pointer()
                .hover(|this| this.opacity(0.9))
                .child(rust_i18n::t!("pull.ready").to_string())
                .on_click(cx.listener(|this, _, _, cx| this.set_draft(false, cx)))
                .into_any_element()
        });
        let to_draft = (!draft && !busy).then(|| {
            h_flex()
                .gap_1()
                .text_size(px(11.5))
                .text_color(muted)
                .child(rust_i18n::t!("pull.still").to_string())
                .child(
                    div()
                        .id("to-draft")
                        .text_color(tokens.colors().accent)
                        .cursor_pointer()
                        .child(rust_i18n::t!("pull.to_draft").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.set_draft(true, cx))),
                )
                .into_any_element()
        });
        let cancel = self.confirm_merge.then(|| {
            self.button(
                "merge-cancel",
                rust_i18n::t!("detail.merge.cancel").to_string(),
                false,
                cx,
                |this, cx| {
                    this.confirm_merge = false;
                    cx.notify();
                },
            )
        });
        let menu = self.merge_menu.then(|| {
            let card = v_flex()
                .id("merge-menu-card")
                .w(px(400.))
                .rounded(px(tokens.radius.panel))
                .bg(tokens.colors().popover())
                .border_1()
                .border_color(tokens.colors().border_strong)
                .shadow_lg()
                .overflow_hidden()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.merge_menu = false;
                    cx.notify();
                }))
                .children(
                    MergeMethod::ALL
                        .iter()
                        .enumerate()
                        .map(|(index, candidate)| {
                            let candidate = *candidate;
                            let (title, desc) = match candidate {
                                MergeMethod::Merge => {
                                    ("merge.method.merge.title", "merge.method.merge.desc")
                                }
                                MergeMethod::Squash => {
                                    ("merge.method.squash.title", "merge.method.squash.desc")
                                }
                                MergeMethod::Rebase => {
                                    ("merge.method.rebase.title", "merge.method.rebase.desc")
                                }
                            };
                            self.picker_row(
                                ("merge-method", index),
                                div().into_any_element(),
                                rust_i18n::t!(title).to_string(),
                                Some(rust_i18n::t!(desc).to_string()),
                                candidate == method,
                                cx,
                                move |this, cx| {
                                    this.merge_method = candidate;
                                    this.merge_menu = false;
                                    this.confirm_merge = false;
                                    cx.notify();
                                },
                            )
                        }),
                );
            div().absolute().top(px(34.)).left_0().child(
                deferred(
                    anchored()
                        .position_mode(AnchoredPositionMode::Local)
                        .snap_to_window_with_margin(px(8.))
                        .child(card),
                )
                .with_priority(1),
            )
        });

        Some(
            v_flex()
                .w_full()
                .rounded(px(tokens.radius.panel))
                .bg(tokens.colors().bg_surface)
                .border_1()
                .border_color(match overall {
                    Some(CheckState::Failure) => red.opacity(0.5),
                    Some(CheckState::Success) if !open || pull.mergeable == Some(true) => {
                        green.opacity(0.5)
                    }
                    _ => tokens.colors().border_subtle,
                })
                .overflow_hidden()
                .child(checks_row)
                .children(runs)
                // A merged or closed pull keeps its checks — the logs are
                // where a red run is explained — and loses the merge.
                .when(open, |this| {
                    this.child(div().h_px().w_full().bg(tokens.colors().border_subtle))
                        .child(conflicts_row)
                        .child(div().h_px().w_full().bg(tokens.colors().border_subtle))
                        .child(
                            v_flex()
                                .w_full()
                                .px_4()
                                .py_3()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .relative()
                                        .gap_2()
                                        .items_center()
                                        .when(!draft, |this| this.child(merge_button))
                                        .children(ready)
                                        .children(cancel)
                                        .children(to_draft),
                                )
                                .children(menu),
                        )
                })
                .into_any_element(),
        )
    }

    /// An item, in either of its tabs.
    fn item(&self, key: ItemKey, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let fetch = self.store.read(cx).detail(&key).cloned();
        let (detail, error, loading) = match &fetch {
            Some(fetch) => (
                fetch.value().cloned(),
                fetch.error().map(str::to_string),
                fetch.is_loading(),
            ),
            None => (None, None, false),
        };
        let Some(detail) = detail else {
            return match error {
                Some(error) => self.notice(error, true, cx),
                None if loading => crate::skeleton::detail(cx),
                None => self.notice(rust_i18n::t!("detail.empty").to_string(), false, cx),
            };
        };

        let item = &detail.item;
        let glyph = Glyph::for_item(item);
        let glyph_color = glyph.role().color(tokens.colors());
        let muted = tokens.colors().text_muted;
        let labels: Vec<LabelChip> = item
            .labels
            .iter()
            .map(|label| LabelChip::new(&label.name, &label.color, muted))
            .collect();
        let tabs = detail
            .pull
            .as_ref()
            .map(|pull| self.tabs(pull.changed_files, cx));
        let showing_files = self.tab == Tab::Files && detail.pull.is_some();
        let project_back = self.project_return.as_ref().map(|(project, _)| {
            let label = rust_i18n::t!("project.back", project = project.title.clone()).to_string();
            h_flex()
                .id("project-back")
                .self_start()
                .gap_1()
                .items_center()
                .px_2()
                .py_1()
                .rounded(px(tokens.radius.control()))
                .cursor_pointer()
                .text_size(px(11.5))
                .text_color(tokens.colors().text_secondary)
                .hover(|this| this.bg(tokens.colors().row_hover()))
                .child(Icon::new(IconName::ChevronLeft).size_3p5())
                .child(label)
                .on_click(cx.listener(|this, _, _, cx| this.return_to_project(cx)))
        });

        let head = v_flex()
            .w_full()
            .px_5()
            .pt_4()
            .pb_3()
            .gap_2()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .children(project_back)
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(muted)
                            .child(format!("#{}", item.number)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(14.))
                            .font_medium()
                            .text_color(tokens.colors().text_primary)
                            .child(item.title.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .px_2()
                            .py_0p5()
                            .rounded(px(tokens.radius.row))
                            .bg(glyph_color.opacity(0.18))
                            .child(
                                Icon::empty()
                                    .path(glyph.icon())
                                    .size_3p5()
                                    .text_color(glyph_color),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .font_medium()
                                    .text_color(glyph_color)
                                    .child(rust_i18n::t!(glyph.label_key()).to_string()),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(avatar(
                                self.store.read(cx).avatar(&item.author.avatar_url),
                                &item.author.login,
                                px(18.),
                                cx,
                            ))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(tokens.colors().text_secondary)
                                    .child(format!(
                                        "{} · {}",
                                        item.author.login,
                                        age(Utc::now(), item.created_at)
                                    )),
                            ),
                    )
                    .children(detail.pull.as_ref().map(|pull| {
                        h_flex()
                            .gap_3()
                            .items_center()
                            .text_size(px(11.5))
                            .child(
                                div()
                                    .px_1p5()
                                    .rounded(px(tokens.radius.row))
                                    .bg(tokens.colors().code_bg)
                                    .text_color(tokens.colors().text_secondary)
                                    .child(
                                        rust_i18n::t!(
                                            "detail.wants_to_merge",
                                            head = pull.head,
                                            base = pull.base
                                        )
                                        .to_string(),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(tokens.colors().status_done)
                                    .child(format!("+{}", pull.additions)),
                            )
                            .child(
                                div()
                                    .text_color(tokens.colors().status_error)
                                    .child(format!("−{}", pull.deletions)),
                            )
                            .child(
                                div().text_color(muted).child(
                                    rust_i18n::t!("detail.files", count = pull.changed_files)
                                        .to_string(),
                                ),
                            )
                    })),
            )
            .child(self.actions(&key, &detail, cx))
            .children(tabs);

        let body: AnyElement = if showing_files {
            self.diff_list(&key, mono, cx)
        } else {
            let comments: Vec<AnyElement> = detail
                .comments
                .iter()
                .map(|comment| self.comment(comment, cx))
                .collect();
            let merge_card = self.merge_card(&key, &detail, cx);
            let facets = self.facets(&key, &detail, &labels, cx);
            v_flex()
                .id("detail-scroll")
                .flex_1()
                // Without a floor of zero the scroll takes its content's
                // height and pushes the composer under the window.
                .min_h_0()
                .overflow_y_scroll()
                .px_5()
                .py_4()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(MEASURE))
                        .gap_4()
                        .child(facets)
                        .child(div().h_px().w_full().bg(tokens.colors().border_subtle))
                        .children(merge_card)
                        .child(if item.body.trim().is_empty() {
                            div()
                                .text_size(px(13.))
                                .text_color(muted)
                                .child(rust_i18n::t!("detail.no_body").to_string())
                                .into_any_element()
                        } else {
                            div()
                                .text_size(px(14.))
                                .line_height(relative(1.6))
                                .child(
                                    TextView::markdown(
                                        SharedString::from(format!(
                                            "body:{}/{}",
                                            item.repo, item.number
                                        )),
                                        item.body.clone(),
                                    )
                                    .selectable(true),
                                )
                                .into_any_element()
                        })
                        .when(!comments.is_empty(), |this| {
                            this.child(div().h_px().w_full().bg(tokens.colors().border_subtle))
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(muted)
                                        .child(rust_i18n::t!("detail.comments").to_string()),
                                )
                                .children(comments)
                        }),
                )
                .into_any_element()
        };
        // On the files too: a review is written while reading the diff,
        // and sending it from the other tab means going back for it.
        let composer = self.composer(item.is_pull(), showing_files, cx);

        v_flex()
            .size_full()
            .child(head)
            .child(body)
            .child(composer)
            .into_any_element()
    }

    /// The comment box at the foot of the column, always in view: a box
    /// that scrolled away with the thread had its button below the fold
    /// more often than not. For a pull the same words can be a review —
    /// approving, or asking for changes — so those are here too.
    ///
    /// It is under the diff as well as under the conversation. A review is
    /// written while reading the diff, and approving from the other tab
    /// means leaving the thing being approved to do it. Under the diff the
    /// words are called a review, because that is what they will be.
    fn composer(&self, is_pull: bool, reviewing: bool, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let mut buttons = h_flex().w_full().justify_end().gap_1p5().items_center();
        if is_pull {
            buttons = buttons
                .child(self.button(
                    "request-changes",
                    rust_i18n::t!("detail.review.request_changes").to_string(),
                    false,
                    cx,
                    |this, cx| this.send_review(ReviewEvent::RequestChanges, cx),
                ))
                .child(self.button(
                    "approve",
                    rust_i18n::t!("detail.review.approve").to_string(),
                    false,
                    cx,
                    |this, cx| this.send_review(ReviewEvent::Approve, cx),
                ));
        }
        buttons = buttons.child(
            self.button(
                "send-comment",
                rust_i18n::t!(if reviewing {
                    "detail.review.comment"
                } else {
                    "detail.comment.send"
                })
                .to_string(),
                true,
                cx,
                |this, cx| this.send_comment(cx),
            ),
        );
        div()
            .w_full()
            .flex_shrink_0()
            .px_5()
            .pb_4()
            .pt_2()
            .border_t_1()
            .border_color(tokens.colors().border_subtle)
            .children(self.review_says.clone().map(|says| {
                div()
                    .w_full()
                    .max_w(px(MEASURE))
                    .pb_1()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().status_attention)
                    .child(says)
            }))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(MEASURE))
                    .rounded(px(tokens.radius.control() + 2.))
                    .bg(tokens.colors().bg_surface)
                    .border_1()
                    .border_color(tokens.colors().border_subtle)
                    .p_1()
                    .gap_1()
                    .child(Textarea::new(&self.composer))
                    .child(buttons.pr_1().pb_0p5()),
            )
            .into_any_element()
    }

    /// One item inside a Project. Issues and pulls lead into the same native
    /// detail this column already uses; draft and redacted items are read-only.
    fn project_item_row(
        &mut self,
        project_id: &str,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let loaded: Option<(ProjectItem, Project)> = self
            .store
            .read(cx)
            .project_board(project_id)
            .and_then(|fetch| fetch.value())
            .and_then(|board| {
                board
                    .items
                    .get(index)
                    .cloned()
                    .map(|item| (item, board.project.clone()))
            });
        let Some((item, project)) = loaded else {
            return div().h(PROJECT_ROW).into_any_element();
        };
        let closed = item.state.as_deref() == Some("CLOSED");
        let (icon_path, icon_color) = match item.kind {
            ProjectItemKind::Issue if closed => {
                (e1_ui::assets::icon::CIRCLE_CHECK, tokens.colors().accent)
            }
            ProjectItemKind::Issue => {
                (e1_ui::assets::icon::CIRCLE_DOT, tokens.colors().status_done)
            }
            ProjectItemKind::PullRequest if closed => (
                e1_ui::assets::icon::PULL_REQUEST_CLOSED,
                tokens.colors().status_error,
            ),
            ProjectItemKind::PullRequest => (
                e1_ui::assets::icon::PULL_REQUEST,
                tokens.colors().status_done,
            ),
            ProjectItemKind::DraftIssue | ProjectItemKind::Redacted => {
                (e1_ui::assets::icon::PROJECT, tokens.colors().text_muted)
            }
        };
        let destination = item
            .item_key()
            .map(|key| (key, item.kind == ProjectItemKind::PullRequest));
        let title = if item.title.is_empty() {
            rust_i18n::t!("project.item.unavailable").to_string()
        } else {
            item.title.clone()
        };
        let subject = match (&item.repo, item.number) {
            (Some(repo), Some(number)) => format!("{repo}#{number}"),
            _ => rust_i18n::t!("project.item.draft").to_string(),
        };
        h_flex()
            .id(("project-item", index))
            .h(PROJECT_ROW)
            .w_full()
            .px_4()
            .gap_2p5()
            .items_center()
            .when(destination.is_some(), |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
            })
            .when_some(destination, |this, (key, is_pull)| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.show_project_item(project.clone(), key.clone(), Some(is_pull), cx)
                }))
            })
            .when(item.archived, |this| this.opacity(0.62))
            .child(
                Icon::empty()
                    .path(icon_path)
                    .size_4()
                    .text_color(icon_color),
            )
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(tokens.colors().text_primary)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().text_muted)
                            .truncate()
                            .child(subject),
                    ),
            )
            .child(
                div()
                    .max_w(px(120.))
                    .px_2()
                    .py_0p5()
                    .rounded(px(tokens.radius.control()))
                    .bg(tokens.colors().row_hover())
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_secondary)
                    .truncate()
                    .child(
                        item.status
                            .unwrap_or_else(|| rust_i18n::t!("project.item.no_status").to_string()),
                    ),
            )
            .into_any_element()
    }

    /// One GitHub-shaped card in a Project board column.
    fn project_board_card(
        &mut self,
        project: &Project,
        project_id: &str,
        item_index: usize,
        field_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let item = self
            .store
            .read(cx)
            .project_board(project_id)
            .and_then(|fetch| fetch.value())
            .and_then(|board| board.items.get(item_index))
            .cloned();
        let Some(item) = item else {
            return div().h(PROJECT_CARD).into_any_element();
        };
        let closed = item.state.as_deref() == Some("CLOSED");
        let (icon_path, icon_color) = match item.kind {
            ProjectItemKind::Issue if closed => {
                (e1_ui::assets::icon::CIRCLE_CHECK, tokens.colors().accent)
            }
            ProjectItemKind::Issue => {
                (e1_ui::assets::icon::CIRCLE_DOT, tokens.colors().status_done)
            }
            ProjectItemKind::PullRequest if closed => (
                e1_ui::assets::icon::PULL_REQUEST_CLOSED,
                tokens.colors().status_error,
            ),
            ProjectItemKind::PullRequest => (
                e1_ui::assets::icon::PULL_REQUEST,
                tokens.colors().status_done,
            ),
            ProjectItemKind::DraftIssue | ProjectItemKind::Redacted => {
                (e1_ui::assets::icon::PROJECT, tokens.colors().text_muted)
            }
        };
        let destination = item
            .item_key()
            .map(|key| (key, item.kind == ProjectItemKind::PullRequest));
        let subject = match (&item.repo, item.number) {
            (Some(repo), Some(number)) => format!("{} #{}", repo.name, number),
            _ => rust_i18n::t!("project.item.draft").to_string(),
        };
        let source_value = item
            .fields
            .iter()
            .find_map(|value| (value.field_id == field_id).then_some(&value.value))
            .and_then(|value| match value {
                e1_github::ProjectValue::SingleSelect(name) => Some(name.clone()),
                _ => None,
            });
        let drag = DraggedProjectItem {
            project: project.clone(),
            item_id: item.id.clone(),
            field_id: field_id.to_string(),
            source_value,
            title: item.title.clone().into(),
        };
        let origin_project = project.clone();
        let fade_id = SharedString::from(format!("project-card:{}", item.id));
        let card = v_flex()
            .id(("project-card", item_index))
            .h(PROJECT_CARD)
            .w_full()
            .my_1()
            .px_3()
            .py_2()
            .gap_1p5()
            .rounded(px(tokens.radius.row))
            .border_1()
            .border_color(tokens.colors().border_subtle)
            .bg(tokens.colors().bg_raised)
            .cursor_pointer()
            .hover(|this| {
                this.border_color(tokens.colors().text_muted)
                    .bg(tokens.colors().row_hover())
            })
            .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
            .when_some(destination, |this, (key, is_pull)| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.show_project_item(origin_project.clone(), key.clone(), Some(is_pull), cx)
                }))
            })
            .child(
                h_flex()
                    .gap_1()
                    .text_size(px(10.5))
                    .text_color(tokens.colors().text_muted)
                    .child(
                        Icon::empty()
                            .path(icon_path)
                            .size_3()
                            .text_color(icon_color),
                    )
                    .child(subject),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_size(px(12.5))
                    .font_medium()
                    .text_color(tokens.colors().text_primary)
                    .child(item.title),
            )
            .into_any_element();
        crate::fade::fade_in(fade_id, card, cx)
    }

    /// Cards arranged using a saved board view's vertical grouping field.
    fn project_board_view(
        &mut self,
        board: &e1_github::ProjectBoard,
        view: &ProjectView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let project_id = board.project.id.clone();
        let project = board.project.clone();
        let field_id = view
            .vertical_group_by
            .clone()
            .or_else(|| {
                board
                    .fields
                    .iter()
                    .find(|field| field.name.eq_ignore_ascii_case("status"))
                    .map(|field| field.id.clone())
            })
            .unwrap_or_default();
        let columns = board_columns(board, view);
        h_flex()
            .size_full()
            .min_w_0()
            .items_start()
            .gap_3()
            .p_3()
            .overflow_x_scrollbar()
            .children(
                columns
                    .into_iter()
                    .enumerate()
                    .map(|(column_index, column)| {
                        let indices = column.items;
                        let count = indices.len();
                        let id = project_id.clone();
                        let card_project = project.clone();
                        let card_field = field_id.clone();
                        let target_name = column.name.clone();
                        let target_option = column.option_id.clone();
                        let drop_project = project.clone();
                        let drop_field = field_id.clone();
                        let drop_accent = tokens.colors().accent;
                        let this = cx.entity();
                        v_flex()
                            .h_full()
                            .w(px(300.))
                            .flex_shrink_0()
                            .rounded(px(tokens.radius.row + 1.))
                            .border_1()
                            .border_color(tokens.colors().border_subtle)
                            .bg(tokens.colors().bg_sidebar)
                            .drag_over::<DraggedProjectItem>(move |column, _, _, _| {
                                column.border_color(drop_accent)
                            })
                            .on_drop(cx.listener(
                                move |this, dragged: &DraggedProjectItem, _, cx| {
                                    if dragged.project.id != drop_project.id
                                        || dragged.field_id != drop_field
                                        || dragged.source_value == target_name
                                    {
                                        return;
                                    }
                                    let option = target_option.clone().zip(target_name.clone());
                                    this.store.update(cx, |store, cx| {
                                        store.move_project_item(
                                            drop_project.clone(),
                                            dragged.item_id.clone(),
                                            drop_field.clone(),
                                            option,
                                            cx,
                                        )
                                    });
                                },
                            ))
                            .child(
                                h_flex()
                                    .h(px(42.))
                                    .flex_shrink_0()
                                    .px_3()
                                    .gap_2()
                                    .border_b_1()
                                    .border_color(tokens.colors().border_subtle)
                                    .text_size(px(12.))
                                    .font_semibold()
                                    .text_color(tokens.colors().text_secondary)
                                    .child(
                                        div()
                                            .size(px(10.))
                                            .rounded_full()
                                            .border_2()
                                            .border_color(tokens.colors().accent),
                                    )
                                    .child(column.name.unwrap_or_else(|| {
                                        rust_i18n::t!("project.item.no_status").to_string()
                                    }))
                                    .child(
                                        div()
                                            .px_1p5()
                                            .rounded_full()
                                            .bg(tokens.colors().row_hover())
                                            .text_color(tokens.colors().text_muted)
                                            .child(count.to_string()),
                                    )
                                    .child(
                                        div()
                                            .ml_auto()
                                            .text_color(tokens.colors().text_muted)
                                            .child("•••"),
                                    ),
                            )
                            .child(
                                uniform_list(
                                    ("project-board-column", column_index),
                                    count,
                                    move |range, _window, cx| {
                                        this.update(cx, |this, cx| {
                                            range
                                                .map(|row| {
                                                    this.project_board_card(
                                                        &card_project,
                                                        &id,
                                                        indices[row],
                                                        &card_field,
                                                        cx,
                                                    )
                                                })
                                                .collect()
                                        })
                                    },
                                )
                                .flex_1()
                                .min_h_0()
                                .px_1()
                                .w_full(),
                            )
                    }),
            )
            .into_any_element()
    }

    /// One virtualized row of the native roadmap.
    fn project_roadmap_row(
        &mut self,
        project_id: &str,
        row: &RoadmapRow,
        extent_start: chrono::NaiveDate,
        extent_end: chrono::NaiveDate,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const LABEL_WIDTH: f32 = 210.;
        const TRACK_WIDTH: f32 = 620.;
        let tokens = Tokens::global(cx).clone();
        let loaded = self
            .store
            .read(cx)
            .project_board(project_id)
            .and_then(|fetch| fetch.value())
            .and_then(|board| {
                board
                    .items
                    .get(row.item)
                    .cloned()
                    .map(|item| (item, board.project.clone()))
            });
        let Some((item, project)) = loaded else {
            return div().h(PROJECT_ROW).into_any_element();
        };
        let days = (extent_end - extent_start).num_days().max(1) as f32 + 1.;
        let left = ((row.start - extent_start).num_days() as f32 / days) * TRACK_WIDTH;
        let width = ((((row.end - row.start).num_days() + 1) as f32 / days) * TRACK_WIDTH).max(8.);
        let destination = item
            .item_key()
            .map(|key| (key, item.kind == ProjectItemKind::PullRequest));
        let state_icon = if item.kind == ProjectItemKind::PullRequest {
            e1_ui::assets::icon::PULL_REQUEST
        } else {
            e1_ui::assets::icon::CIRCLE_DOT
        };
        h_flex()
            .id(("project-roadmap-item", row.item))
            .h(PROJECT_ROW)
            .w(px(LABEL_WIDTH + TRACK_WIDTH + 32.))
            .px_4()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .when(destination.is_some(), |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(tokens.colors().row_hover()))
            })
            .when_some(destination, |this, (key, is_pull)| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.show_project_item(project.clone(), key.clone(), Some(is_pull), cx)
                }))
            })
            .child(
                v_flex()
                    .w(px(LABEL_WIDTH))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Icon::empty()
                                    .path(state_icon)
                                    .size_3()
                                    .text_color(tokens.colors().status_done),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .text_color(tokens.colors().text_primary)
                                    .child(item.title),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(tokens.colors().text_muted)
                            .child(format!("{} – {}", row.start, row.end)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w(px(TRACK_WIDTH))
                    .h(px(22.))
                    .flex_shrink_0()
                    .rounded(px(tokens.radius.control()))
                    .bg(tokens.colors().bg_sidebar)
                    .children((1..12).map(|tick| {
                        div()
                            .absolute()
                            .left(px(TRACK_WIDTH * tick as f32 / 12.))
                            .top_0()
                            .h_full()
                            .border_l_1()
                            .border_color(tokens.colors().border_subtle)
                    }))
                    .child(
                        div()
                            .absolute()
                            .left(px(left))
                            .top(px(3.))
                            .h(px(16.))
                            .w(px(width))
                            .rounded(px(tokens.radius.control()))
                            .border_1()
                            .border_color(tokens.colors().accent)
                            .bg(tokens.colors().row_active()),
                    ),
            )
            .into_any_element()
    }

    /// A saved roadmap view over date and iteration values.
    fn project_roadmap_view(
        &mut self,
        board: &e1_github::ProjectBoard,
        view: &ProjectView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const LABEL_WIDTH: f32 = 210.;
        const TRACK_WIDTH: f32 = 620.;
        let tokens = Tokens::global(cx).clone();
        let Some(roadmap) = roadmap(board, view) else {
            return self.notice(
                rust_i18n::t!("project.roadmap.no_dates").to_string(),
                false,
                cx,
            );
        };
        let count = roadmap.rows.len();
        let rows = roadmap.rows;
        let start = roadmap.start;
        let end = roadmap.end;
        let project_id = board.project.id.clone();
        let this = cx.entity();
        v_flex()
            .size_full()
            .min_w(px(LABEL_WIDTH + TRACK_WIDTH + 32.))
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(div().w(px(LABEL_WIDTH)))
                    .child(
                        h_flex()
                            .w(px(TRACK_WIDTH))
                            .justify_between()
                            .text_size(px(10.5))
                            .text_color(tokens.colors().text_muted)
                            .child(start.to_string())
                            .child(end.to_string()),
                    ),
            )
            .child(
                uniform_list("project-roadmap", count, move |range, _window, cx| {
                    this.update(cx, |this, cx| {
                        range
                            .map(|index| {
                                this.project_roadmap_row(&project_id, &rows[index], start, end, cx)
                            })
                            .collect()
                    })
                })
                .flex_1()
                .min_h_0()
                .w_full(),
            )
            .into_any_element()
    }

    /// A Project's metadata and every item in its default project order.
    fn project(&mut self, project: Project, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let fetch = self.store.read(cx).project_board(&project.id).cloned();
        let loading = fetch.as_ref().is_some_and(|fetch| fetch.is_loading());
        let error = fetch
            .as_ref()
            .and_then(|fetch| fetch.error())
            .map(str::to_string);
        let board = fetch.as_ref().and_then(|fetch| fetch.value()).cloned();
        let count = board.as_ref().map(|board| board.items.len());
        let project_action = self.store.read(cx).project_action(&project.id).cloned();
        let selected_view = board.as_ref().and_then(|board| {
            self.project_view
                .as_deref()
                .and_then(|id| board.views.iter().find(|view| view.id == id))
                .or_else(|| board.views.first())
                .cloned()
        });
        let visible_count = board
            .as_ref()
            .zip(selected_view.as_ref())
            .map(|(board, view)| visible_item_indices(board, view).len())
            .or(count);
        let view_chips = board.as_ref().and_then(|board| {
            (!board.views.is_empty()).then(|| {
                let selected_id = selected_view.as_ref().map(|view| view.id.as_str());
                h_flex()
                    .w_full()
                    .gap_0p5()
                    .overflow_x_scrollbar()
                    .children(board.views.iter().map(|view| {
                        let selected = selected_id == Some(view.id.as_str());
                        let id = view.id.clone();
                        div()
                            .id(("project-view", view.number as usize))
                            .flex_shrink_0()
                            .px_3()
                            .py_2()
                            .border_b_2()
                            .border_color(if selected {
                                tokens.colors().accent
                            } else {
                                transparent_black()
                            })
                            .cursor_pointer()
                            .text_size(px(11.5))
                            .when(selected, |this| {
                                this.text_color(tokens.colors().text_primary)
                                    .font_semibold()
                            })
                            .when(!selected, |this| {
                                this.text_color(tokens.colors().text_muted)
                            })
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .child(view.name.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.project_view = Some(id.clone());
                                cx.notify();
                            }))
                    }))
                    .into_any_element()
            })
        });
        let filter_bar = selected_view.as_ref().map(|view| {
            h_flex()
                .w_full()
                .h(px(34.))
                .px_3()
                .gap_2()
                .rounded(px(tokens.radius.control()))
                .border_1()
                .border_color(tokens.colors().border_subtle)
                .bg(tokens.colors().bg_surface)
                .text_size(px(11.5))
                .text_color(tokens.colors().text_secondary)
                .child(
                    Icon::new(IconName::Search)
                        .size_3()
                        .text_color(tokens.colors().text_muted),
                )
                .child(
                    view.filter
                        .clone()
                        .unwrap_or_else(|| rust_i18n::t!("project.filter.empty").to_string()),
                )
                .child(
                    div()
                        .ml_auto()
                        .px_1p5()
                        .rounded_full()
                        .bg(tokens.colors().row_hover())
                        .text_color(tokens.colors().text_muted)
                        .child(visible_count.unwrap_or_default().to_string()),
                )
        });
        let head = v_flex()
            .w_full()
            .px_5()
            .py_4()
            .gap_2()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                div()
                    .text_size(px(16.))
                    .font_semibold()
                    .text_color(tokens.colors().text_primary)
                    .child(project.title.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(project.owner.clone())
                    .child(format!("#{}", project.number))
                    .children(
                        count
                            .map(|count| rust_i18n::t!("project.items", count = count).to_string()),
                    )
                    .when(loading && count.is_some(), |this| {
                        this.child(spinner(px(12.), tokens.colors().text_muted))
                    }),
            )
            .children(view_chips)
            .children(filter_bar)
            .children(project_action.as_ref().and_then(|action| {
                action.error().map(|error| {
                    div()
                        .text_size(px(11.5))
                        .text_color(tokens.colors().status_error)
                        .child(error.to_string())
                })
            }));
        let body: AnyElement = match (count, error, loading) {
            (Some(0), _, _) => {
                self.notice(rust_i18n::t!("project.items.empty").to_string(), false, cx)
            }
            (Some(count), _, _) => match (board.as_ref(), selected_view.as_ref()) {
                (Some(board), Some(view)) if view.layout == ProjectViewLayout::Board => {
                    self.project_board_view(board, view, cx)
                }
                (Some(board), Some(view)) if view.layout == ProjectViewLayout::Roadmap => div()
                    .size_full()
                    .overflow_x_scrollbar()
                    .child(self.project_roadmap_view(board, view, cx))
                    .into_any_element(),
                _ => {
                    let id = project.id.clone();
                    let indices = board
                        .as_ref()
                        .zip(selected_view.as_ref())
                        .map(|(board, view)| visible_item_indices(board, view))
                        .unwrap_or_else(|| (0..count).collect());
                    let row_count = indices.len();
                    let this = cx.entity();
                    uniform_list("project-items", row_count, move |range, _window, cx| {
                        this.update(cx, |this, cx| {
                            range
                                .map(|index| this.project_item_row(&id, indices[index], cx))
                                .collect()
                        })
                    })
                    .flex_1()
                    .size_full()
                    .into_any_element()
                }
            },
            (None, Some(error), _) => self.notice(error, true, cx),
            (None, None, true) => crate::skeleton::project_rows(7, cx),
            (None, None, false) => {
                self.notice(rust_i18n::t!("project.items.empty").to_string(), false, cx)
            }
        };
        v_flex()
            .size_full()
            .child(head)
            .child(body)
            .into_any_element()
    }

    /// A file out of the tree.
    fn file(&self, key: FileKey, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let fetch = self.store.read(cx).content(&key).cloned();
        let (content, error) = match &fetch {
            Some(fetch) => (fetch.value().cloned(), fetch.error().map(str::to_string)),
            None => (None, None),
        };
        let head = v_flex()
            .w_full()
            .px_5()
            .pt_4()
            .pb_3()
            .gap_1()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                div()
                    .text_size(px(13.))
                    .font_family(mono.clone())
                    .text_color(tokens.colors().text_primary)
                    .child(key.1.clone()),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(key.0.to_string())
                    .children(content.as_ref().map(|content| {
                        rust_i18n::t!("file.bytes", count = content.size).to_string()
                    }))
                    .when(!self.lines.is_empty(), |this| {
                        this.child(
                            rust_i18n::t!("file.lines", count = self.lines.len()).to_string(),
                        )
                    })
                    .children(self.code.as_ref().map(|code| code.language().to_string())),
            );

        let body: AnyElement = match content {
            Some(content) if content.text.is_some() => {
                let this = cx.entity();
                uniform_list("code", self.lines.len(), move |range, _window, cx| {
                    this.update(cx, |this, cx| {
                        range
                            .map(|index| this.code_row(index, mono.clone(), cx))
                            .collect()
                    })
                })
                .flex_1()
                .size_full()
                .py_1()
                .into_any_element()
            }
            Some(_) => self.notice(rust_i18n::t!("file.too_large").to_string(), false, cx),
            None => match error {
                Some(error) => self.notice(error, true, cx),
                None => crate::skeleton::diff(cx),
            },
        };
        v_flex()
            .size_full()
            .child(head)
            .child(body)
            .into_any_element()
    }
}

impl Detail {
    /// Hand the current context to the far-right agent pane.
    fn open_ask(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ask) = self.ask(cx)
            && self.store.read(cx).has_chat_agent()
        {
            self.offer_at = None;
            cx.emit(DetailEvent::Ask(ask));
            cx.notify();
        }
    }

    /// One commit: what it says, who wrote it, and what it changed.
    fn commit(&self, repo: RepoId, sha: String, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let fetch = self.store.read(cx).commit(&repo, &sha).cloned();
        let (detail, error) = match &fetch {
            Some(fetch) => (fetch.value().cloned(), fetch.error().map(str::to_string)),
            None => (None, None),
        };
        let Some(detail) = detail else {
            let body = match error {
                Some(error) => self.notice(error, true, cx),
                None => crate::skeleton::diff(cx),
            };
            return v_flex().size_full().child(body).into_any_element();
        };
        let commit = &detail.commit;
        let picture = commit.author.as_ref().map(|author| {
            avatar(
                self.store.read(cx).avatar(&author.avatar_url),
                &author.login,
                px(18.),
                cx,
            )
        });
        let body = commit.body().to_string();
        let head = v_flex()
            .w_full()
            .flex_shrink_0()
            .px_5()
            .pt_4()
            .pb_3()
            .gap_2()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .child(
                div()
                    .text_size(px(15.))
                    .font_medium()
                    .text_color(tokens.colors().text_primary)
                    .child(commit.subject().to_string()),
            )
            .when(!body.is_empty(), |this| {
                this.child(
                    div()
                        .max_w(px(MEASURE))
                        .text_size(px(12.5))
                        .font_family(mono.clone())
                        .line_height(relative(1.5))
                        .text_color(tokens.colors().text_secondary)
                        .child(body),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .children(picture)
                    .child(
                        div()
                            .text_color(tokens.colors().text_secondary)
                            .child(commit.author_name.clone()),
                    )
                    .child(div().child(age(chrono::Utc::now(), commit.authored_at)))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .px_1p5()
                            .rounded(px(tokens.radius.control()))
                            .bg(tokens.colors().code_bg)
                            .child(commit.short().to_string()),
                    )
                    .when(commit.is_merge(), |this| {
                        this.child(
                            div()
                                .px_1p5()
                                .rounded(px(tokens.radius.control()))
                                .bg(tokens.colors().accent.opacity(0.18))
                                .text_color(tokens.colors().accent)
                                .child(rust_i18n::t!("commit.merge").to_string()),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("commit.files", count = detail.files.len()).to_string())
                    .child(
                        div()
                            .text_color(tokens.colors().status_done)
                            .child(format!("+{}", detail.additions)),
                    )
                    .child(
                        div()
                            .text_color(tokens.colors().status_error)
                            .child(format!("−{}", detail.deletions)),
                    )
                    .child(self.diff_modes(cx)),
            );
        let this = cx.entity();
        let body = list(self.diff_state.clone(), move |index, _window, cx| {
            this.update(cx, |this, cx| this.diff_row(index, mono.clone(), cx))
        })
        .flex_1()
        .size_full();
        v_flex()
            .size_full()
            .child(head)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// An Actions job's log: the job's name, then its lines.
    fn log(&self, repo: RepoId, job: u64, name: String, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let fetch = self.store.read(cx).log(&repo, job).cloned();
        let (text, error) = match &fetch {
            Some(fetch) => (fetch.value().cloned(), fetch.error().map(str::to_string)),
            None => (None, None),
        };
        let head = v_flex()
            .w_full()
            .px_5()
            .pt_3()
            .pb_3()
            .gap_1()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .when(self.log_previous.is_some(), |this| {
                this.child(
                    h_flex()
                        .id("log-back")
                        .w_auto()
                        .self_start()
                        .mb_1()
                        .gap_0p5()
                        .items_center()
                        .text_size(px(11.5))
                        .text_color(tokens.colors().accent)
                        .cursor_pointer()
                        .child(Icon::new(IconName::ChevronLeft).size_3p5())
                        .child(rust_i18n::t!("log.back").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
                )
            })
            .child(
                div()
                    .text_size(px(13.))
                    .font_medium()
                    .text_color(tokens.colors().text_primary)
                    .child(name),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_size(px(11.5))
                    .text_color(tokens.colors().text_muted)
                    .child(repo.to_string())
                    .child(rust_i18n::t!("log.title", id = job).to_string())
                    .when(!self.log_lines.is_empty(), |this| {
                        this.child(
                            rust_i18n::t!("log.lines", count = self.log_lines.len()).to_string(),
                        )
                    }),
            );
        let body: AnyElement = match (text, error) {
            (Some(_), _) => {
                let this = cx.entity();
                list(self.log_state.clone(), move |index, _window, cx| {
                    this.update(cx, |this, cx| this.log_row(index, mono.clone(), cx))
                })
                .flex_1()
                .size_full()
                .py_1()
                .into_any_element()
            }
            (None, Some(error)) => self.notice(error, true, cx),
            (None, None) => crate::skeleton::diff(cx),
        };
        v_flex()
            .size_full()
            .child(head)
            .child(body)
            .into_any_element()
    }
}

impl Render for Detail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_composer {
            self.clear_composer = false;
            self.composer
                .update(cx, |composer, cx| composer.set_value("", window, cx));
        }
        if self.clear_filter {
            self.clear_filter = false;
            self.filter
                .update(cx, |filter, cx| filter.set_value("", window, cx));
        }
        if self.clear_review {
            self.clear_review = false;
            self.review_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        // What is on screen fades in when it lands. The id is what the
        // content is, so the fade plays once per thing read and not again
        // while it is being read.
        let (body, id): (AnyElement, ElementId) = match self.showing.clone() {
            None => (
                self.notice(rust_i18n::t!("detail.empty").to_string(), false, cx),
                "detail-empty".into(),
            ),
            Some(Showing::Item(key)) => (
                self.item(key.clone(), cx),
                (
                    SharedString::from(format!("detail-item:{}", key.0)),
                    key.1 as usize,
                )
                    .into(),
            ),
            Some(Showing::File(key)) => (
                self.file(key.clone(), cx),
                SharedString::from(format!("detail-file:{}/{}", key.0, key.1)).into(),
            ),
            Some(Showing::Log { repo, job, name }) => {
                (self.log(repo, job, name, cx), ("detail-log", job).into())
            }
            Some(Showing::Commit { repo, sha }) => (
                self.commit(repo, sha.clone(), cx),
                SharedString::from(format!("detail-commit:{sha}")).into(),
            ),
            Some(Showing::Project(project)) => (
                self.project(project.clone(), cx),
                SharedString::from(format!("detail-project:{}", project.id)).into(),
            ),
        };
        let body = crate::fade::fade_in(id, body, cx);
        // The scripted demo can request the same Ask action as the button.
        // Only offer it once a CLI has been discovered.
        if self.ask_soon && self.store.read(cx).has_chat_agent() {
            // Opening the sibling pane while this child is rendering would
            // mutate the shell mid-draw, so emit on the next frame.
            self.ask_soon = false;
            let this = cx.entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |this, cx| this.open_ask(window, cx));
            });
        }
        v_flex()
            .relative()
            .size_full()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.notice_selection(event.position, window, cx)
                }),
            )
            .child(div().flex_1().min_h_0().child(body))
            .children(self.picked_offer(cx))
    }
}
