//! Interactive previews of bounded image bytes supplied by the daemon.

use crate::{Tokens, editor::image_data_url, image_viewport::ImageViewport};
use base64::Engine as _;
use ginka_protocol::model::FileContent;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Sizable as _, h_flex, v_flex};
use std::io::Cursor;

/// One file tab's image, zoom, focus and drag position. No disk or network reads.
pub struct ImagePreview {
    source: SharedString,
    geometry: ImageViewport,
    bounds: Bounds<Pixels>,
    focus: FocusHandle,
    drag: Option<Point<Pixels>>,
}

impl ImagePreview {
    /// Build a preview only for complete, supported images with readable headers.
    pub fn from_file(file: &FileContent, cx: &mut App) -> Option<Self> {
        let source = image_data_url(file)?;
        let encoded = &file.image.as_ref()?.data_base64;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let (width, height) = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self {
            source: source.into(),
            geometry: ImageViewport::new(width as f32, height as f32),
            bounds: Bounds::default(),
            focus: cx.focus_handle(),
            drag: None,
        })
    }

    fn zoom_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.geometry.zoom_at(
            factor,
            f32::from(self.bounds.size.width) / 2.,
            f32::from(self.bounds.size.height) / 2.,
        );
        cx.notify();
    }

    fn zoom_cursor(&mut self, factor: f32, position: Point<Pixels>, cx: &mut Context<Self>) {
        let local = position - self.bounds.origin;
        self.geometry
            .zoom_at(factor, f32::from(local.x), f32::from(local.y));
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        }
        match key.key.as_str() {
            "+" | "=" => self.zoom_center(1.25, cx),
            "-" => self.zoom_center(0.8, cx),
            "0" => {
                self.geometry.reset();
                cx.notify();
            }
            "left" => {
                self.geometry.pan(40., 0.);
                cx.notify();
            }
            "right" => {
                self.geometry.pan(-40., 0.);
                cx.notify();
            }
            "up" => {
                self.geometry.pan(0., 40.);
                cx.notify();
            }
            "down" => {
                self.geometry.pan(0., -40.);
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Focusable for ImagePreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let rect = self.geometry.rect();
        let measured = cx.entity().downgrade();
        v_flex()
            .id("file-image-preview")
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .child(
                        Button::new("image-zoom-out")
                            .ghost()
                            .small()
                            .label(rust_i18n::t!("surface.files.image.zoom_out").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.zoom_center(0.8, cx))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(format!("{:.0}%", self.geometry.zoom() * 100.)),
                    )
                    .child(
                        Button::new("image-zoom-in")
                            .ghost()
                            .small()
                            .label(rust_i18n::t!("surface.files.image.zoom_in").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.zoom_center(1.25, cx))),
                    )
                    .child(
                        Button::new("image-fit")
                            .ghost()
                            .small()
                            .label(rust_i18n::t!("surface.files.image.fit").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.geometry.reset();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("image-viewport")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .track_focus(&self.focus)
                    .tab_stop(true)
                    .border_1()
                    .border_color(if self.focus.is_focused(window) {
                        tokens.colors().accent
                    } else {
                        tokens.colors().border_subtle
                    })
                    .capture_key_down(cx.listener(Self::key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.focus.focus(window, cx);
                            if event.click_count >= 2 {
                                this.geometry.reset();
                                this.drag = None;
                            } else {
                                this.drag = Some(event.position);
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if !event.dragging() {
                            this.drag = None;
                            return;
                        }
                        if let Some(previous) = this.drag {
                            this.drag = Some(event.position);
                            let delta = event.position - previous;
                            this.geometry.pan(f32::from(delta.x), f32::from(delta.y));
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.drag = None),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        let delta = match event.delta {
                            ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
                            ScrollDelta::Lines(delta) => (delta.x * 24., delta.y * 24.),
                        };
                        if event.modifiers.control || event.modifiers.platform {
                            this.zoom_cursor(
                                (delta.1 * 0.01).clamp(-1., 1.).exp(),
                                event.position,
                                cx,
                            );
                        } else {
                            this.geometry.pan(delta.0, delta.1);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| {
                        this.zoom_cursor(1. + event.delta, event.position, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        img(self.source.clone())
                            .absolute()
                            .left(px(rect.x))
                            .top(px(rect.y))
                            .w(px(rect.width))
                            .h(px(rect.height))
                            .object_fit(ObjectFit::Contain),
                    )
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                let _ = measured.update(cx, |this, cx| {
                                    if this.bounds != bounds {
                                        this.bounds = bounds;
                                        this.geometry.resize(
                                            f32::from(bounds.size.width),
                                            f32::from(bounds.size.height),
                                        );
                                        cx.notify();
                                    }
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(tokens.colors().text_muted)
                    .child(rust_i18n::t!("surface.files.image.help").to_string()),
            )
    }
}
