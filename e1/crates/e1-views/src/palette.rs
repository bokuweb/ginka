//! The palette: one field over everywhere the window can go (`docs/ui.md`
//! §3.6).
//!
//! A dialog rather than a box in the centre strip, which is where the search
//! started: the strip belongs to a repository — its tabs, its open/closed
//! toggle — and this goes everywhere, so a fixed-width field there was both
//! in the wrong place and the first thing a narrow window cut off. What it
//! offers for what has been typed is `e1_ui::palette`; this draws it, over
//! the toolkit's own `Command`, which owns the keyboard: ↑↓ walk the rows
//! while the field keeps the caret, ⏎ takes the highlighted one, and Escape
//! clears the query before it closes the dialog.

use crate::store::{Store, StoreEvent};
use e1_ui::Tokens;
use e1_ui::palette::{self, Block, Pick};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_component::{Icon, IconName, IndexPath, WindowExt as _, h_flex};

/// How many repositories a query may list. Past this the reader types
/// another letter, as in the file finder.
const MATCH_CAP: usize = 200;

/// How tall the list is let grow before it scrolls.
const LIST_HEIGHT: Pixels = px(340.);

/// Emitted when the reader picks a row.
pub enum PaletteEvent {
    /// Go where this row points.
    Pick(Pick),
}

impl EventEmitter<PaletteEvent> for Palette {}

/// The palette's contents.
pub struct Palette {
    store: Entity<Store>,
    state: Entity<CommandState>,
    /// What has been typed, trimmed by the model rather than here.
    query: String,
    /// What is offered for it, by heading.
    blocks: Vec<Block>,
}

impl Palette {
    /// A palette over a store, offering the repositories it knows.
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        // The repositories land after the window opens, and a palette opened
        // in between has to grow them when they do.
        cx.subscribe(&store, |this, _, _: &StoreEvent, cx| this.rebuild(cx))
            .detach();
        let mut this = Self {
            store,
            state,
            query: String::new(),
            blocks: Vec::new(),
        };
        this.rebuild(cx);
        this
    }

    /// Empty the field and offer the window's furniture again.
    ///
    /// Called every time the palette is opened: a query left over from the
    /// last time is a filter nobody asked for.
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.state
            .update(cx, |state, cx| state.set_query("", window, cx));
        self.rebuild(cx);
    }

    /// Take what was typed and offer what matches it.
    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let repos = self
            .store
            .read(cx)
            .repos()
            .value()
            .cloned()
            .unwrap_or_default();
        self.blocks = palette::offer(&repos, &self.query, MATCH_CAP);
        cx.notify();
    }

    /// The row at a path in the model the last render installed.
    fn row(&self, index: IndexPath) -> Option<&palette::Row> {
        self.blocks.get(index.section)?.rows.get(index.row)
    }

    /// One row, drawn the way the sidebar draws the same thing: a section
    /// keeps its own glyph, a repository its owner and its lock.
    fn item(row: &palette::Row, cx: &App) -> CommandItem {
        let tokens = Tokens::global(cx).clone();
        let label = row.label.clone();
        let detail = row.detail.clone();
        let private = row.private;
        let icon = match &row.pick {
            Pick::Section(section) => Icon::empty().path(section.icon()),
            Pick::Repo(_) => Icon::new(IconName::BookOpen),
            Pick::Search(_) => Icon::new(IconName::Search),
        };
        CommandItem::new()
            .label(row.label.clone())
            .child(move |_, _| {
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(icon.clone().size_4().flex_shrink_0())
                    // Both texts may shrink and truncate: a repository's
                    // name can be as long as an owner's, and the search
                    // row's label carries the whole query.
                    .child(div().min_w_0().truncate().child(label.clone()))
                    .children(detail.clone().map(|owner| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(tokens.colors().text_muted)
                            .child(owner)
                    }))
                    .when(private, |this| {
                        this.child(
                            Icon::empty()
                                .path(e1_ui::assets::icon::LOCK)
                                .size_3()
                                .flex_shrink_0()
                                .text_color(tokens.colors().text_muted),
                        )
                    })
            })
    }
}

/// The palette's focus is the field's: opening the dialog takes the focus
/// for itself, and the caret is put back here once it has.
impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Weak: the state holds the model, the model holds these callbacks,
        // and the palette holds the state — a strong handle here would be a
        // cycle that never drops.
        let typed = cx.entity().downgrade();
        let picked = cx.entity().downgrade();
        let mut command = Command::new(&self.state)
            // The matching is `e1_ui::palette`'s, over `nucleo`: the
            // toolkit's own filter is a substring of the label, which would
            // drop the search row the moment nothing else matched.
            .filterable(false)
            .bordered(false)
            .placeholder(rust_i18n::t!("palette.placeholder").to_string())
            .max_h(LIST_HEIGHT)
            // No empty slot: the sections always match, so the list is never
            // empty, and anything typed carries the search row even when
            // nothing local matches it.
            .on_query(move |query, _, cx| {
                let query = query.to_string();
                typed.update(cx, |this, cx| this.set_query(query, cx)).ok();
            })
            .on_confirm(move |index, window, cx| {
                let Some(pick) = picked
                    .read_with(cx, |this, _| this.row(index).map(|row| row.pick.clone()))
                    .ok()
                    .flatten()
                else {
                    return;
                };
                window.close_dialog(cx);
                picked
                    .update(cx, |_, cx| cx.emit(PaletteEvent::Pick(pick)))
                    .ok();
            });
        for block in &self.blocks {
            let mut group =
                CommandGroup::new().label(rust_i18n::t!(block.group.label_key()).to_string());
            for row in &block.rows {
                group = group.item(Self::item(row, cx));
            }
            command = command.group(group);
        }
        command
    }
}
