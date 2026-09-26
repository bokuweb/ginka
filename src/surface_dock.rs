//! The right panel's dock appearance: the toolkit's own skin for everything
//! but the tab bar, which is drawn here with the app's tab
//! (`ginka_ui::chrome::tab`).
//!
//! The stock tab bar frames the tab in front with full-height rules on both
//! sides, and its toolbar with another. No other strip in the window has
//! them — the conversation tabs, the terminal, the editor are rounded tabs
//! on a clear strip (`docs/ui.md` §2) — and the rules come from the theme's
//! one `border` colour, which cannot be cleared for the dock alone. So the
//! bar is this crate's: the same tabs, a hairline under the strip, and the
//! dock's selection, closing and drag-and-drop driven through the group.

use ginka_ui::Tokens;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{
    AnyDrag, BasePanelView, DockArea, DockAreaRenderer, DockContext, DockSkin, DragPanel,
    DropIndicator, PanelHandle, PanelState, TabGroupContext, TabGroupRenderer, TilesRenderer,
};
use gpui_component::h_flex;
use std::rc::Rc;
use std::sync::Arc;

/// How big the preview following a dragged tab is, told to the dock so a
/// drop placeholder knows where to fly in from.
const DRAG_PREVIEW: Size<Pixels> = size(px(112.), px(ginka_ui::chrome::TAB_HEIGHT));

/// A dock area wearing [`SurfaceSkin`].
///
/// Built the way `DockSkin::dock_area` builds its own: the skin needs the
/// area's weak handle, so it can only be made while the area is.
pub fn dock_area(
    id: impl Into<SharedString>,
    version: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DockArea> {
    cx.new(|cx| {
        let skin = Rc::new(SurfaceSkin {
            inner: DockSkin::new(cx),
        });
        DockArea::new(id, version, window, cx).with_renderer(skin)
    })
}

/// The toolkit's skin with the tab bar swapped out.
struct SurfaceSkin {
    inner: Rc<DockSkin>,
}

impl DockAreaRenderer for SurfaceSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(window, cx)
    }

    fn split_frame(
        &self,
        node: gpui_component::dock::NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.split_frame(node, axis, window, cx)
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.center_frame(window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_dock(dock, content, window, cx)
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        self.inner.build_placeholder(state, window, cx)
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(SurfaceTabs {
            inner: self.inner.tab_group_renderer(),
        })
    }

    fn tiles_renderer(&self) -> Rc<dyn TilesRenderer> {
        self.inner.tiles_renderer()
    }
}

/// A tab group: the toolkit's frame, content and drop placeholder, and this
/// crate's tab bar.
struct SurfaceTabs {
    inner: Rc<dyn TabGroupRenderer>,
}

impl TabGroupRenderer for SurfaceTabs {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(group, window, cx)
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.content_frame(group, window, cx)
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let tokens = Tokens::global(cx).clone();
        let displayed = group.active_panel().map(|panel| panel.panel_id(cx));
        let droppable = group.is_droppable();
        let draggable = group.is_draggable();
        let closable = group.is_closable();
        let drop_tint = tokens.colors().accent.opacity(0.12);
        let visible: Vec<usize> = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .map(|(ix, _)| ix)
            .collect();
        let tabs: Vec<AnyElement> = visible
            .into_iter()
            .map(|ix| {
                let panel = &group.panels()[ix];
                let id = panel.panel_id(cx);
                let active = displayed == Some(id);
                let handle = PanelHandle::of(panel);
                let content = match handle {
                    Some(handle) => handle.title(window, cx),
                    None => SharedString::from(panel.panel_name(cx)).into_any_element(),
                };
                let label: SharedString = handle
                    .and_then(|handle| handle.tab_name(cx))
                    .unwrap_or_else(|| panel.panel_name(cx).into());
                ginka_ui::chrome::tab(("surface-dock-tab", ix), active, cx)
                    .child(content)
                    .on_click({
                        let group = group.clone();
                        move |_, window, cx| group.select_tab(ix, window, cx)
                    })
                    .when(closable && panel.closable(cx), |this| {
                        let group = group.clone();
                        this.child(
                            ginka_ui::chrome::tab_close(("surface-dock-close", ix), cx).on_click(
                                move |_, window, cx| {
                                    cx.stop_propagation();
                                    group.close(id, window, cx);
                                },
                            ),
                        )
                    })
                    .when_some(
                        draggable.then(|| group.drag_panel(ix, cx)).flatten(),
                        |this, drag| {
                            this.on_drag(drag, move |drag, offset, _, cx| {
                                cx.stop_propagation();
                                drag.set_drag_offset(offset);
                                drag.set_preview_size(DRAG_PREVIEW);
                                let label = label.clone();
                                cx.new(|_| DraggedTab { label })
                            })
                        },
                    )
                    .when(droppable, |this| {
                        this.drag_over::<DragPanel>(move |this, _, _, _| this.bg(drop_tint))
                            .on_drop({
                                let group = group.clone();
                                move |drag: &DragPanel, window, cx| {
                                    group.drop_panel(drag.clone(), Some(ix), true, window, cx)
                                }
                            })
                            .drag_over::<AnyDrag>(move |this, _, _, _| this.bg(drop_tint))
                            .on_drop({
                                let group = group.clone();
                                move |item: &AnyDrag, window, cx| {
                                    group.drop_item(item.clone(), None, window, cx)
                                }
                            })
                    })
                    .into_any_element()
            })
            .collect();
        let count = group.panels().len();
        h_flex()
            .id("surface-dock-tabs")
            .w_full()
            .flex_shrink_0()
            .h(px(36.))
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(tokens.colors().border_subtle)
            .overflow_x_scroll()
            .children(tabs)
            // Past the last tab: somewhere to drop a tab so it goes last.
            .child(
                div()
                    .id("surface-dock-tabs-end")
                    .h_full()
                    .flex_1()
                    .min_w_16()
                    .when(droppable, |this| {
                        let node = group.node();
                        this.drag_over::<DragPanel>(move |this, _, _, _| this.bg(drop_tint))
                            .on_drop({
                                let group = group.clone();
                                move |drag: &DragPanel, window, cx| {
                                    // Its own group's tab moves to the end;
                                    // one from elsewhere joins behind.
                                    let ix = (drag.source() == node).then(|| count - 1);
                                    group.drop_panel(drag.clone(), ix, false, window, cx)
                                }
                            })
                            .drag_over::<AnyDrag>(move |this, _, _, _| this.bg(drop_tint))
                            .on_drop({
                                let group = group.clone();
                                move |item: &AnyDrag, window, cx| {
                                    group.drop_item(item.clone(), None, window, cx)
                                }
                            })
                    }),
            )
            .into_any_element()
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_active_panel(panel, group, window, cx)
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_empty(group, window, cx)
    }
}

/// What follows the cursor while a tab is dragged: the tab, lifted.
struct DraggedTab {
    label: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ginka_ui::chrome::tab("surface-dock-dragged", true, cx)
            .w(DRAG_PREVIEW.width)
            .pr(px(8.))
            .opacity(0.85)
            .child(ginka_ui::chrome::tab_label(self.label.clone()))
    }
}
