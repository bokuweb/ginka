//! Native workspace browser and its inspect-to-chat bridge.

#![cfg(any(target_os = "macos", target_os = "windows"))]

use ginka_core::browser::{BrowserCapture, sanitize_capture};
use ginka_ui::Tokens;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{IconName, h_flex, v_flex};
use raw_window_handle::HasWindowHandle as _;
use serde::Deserialize;

/// A browser selection ready to be reviewed or sent to the active agent.
pub enum BrowserEvent {
    /// The page's untrusted payload after Rust-side validation and bounds.
    Inspected(Box<BrowserCapture>),
    /// A page finished loading: history for the address bar.
    Visited { url: String, title: Option<String> },
    /// The address bar's text changed: time to offer pages from history.
    Typed(String),
}

impl EventEmitter<BrowserEvent> for BrowserPane {}

enum BrowserSignal {
    Inspect(String),
}

#[derive(Deserialize)]
struct VisitEnvelope {
    kind: String,
    url: String,
    title: Option<String>,
}

#[derive(Deserialize)]
struct InspectEnvelope {
    kind: String,
    capture: BrowserCapture,
}

/// One native browser view with toolbar state scoped to the selected workspace.
pub struct BrowserPane {
    webview: Entity<gpui_wry::WebView>,
    address: Entity<InputState>,
    inspecting: bool,
    visible: bool,
    complaint: Option<SharedString>,
    /// Pages from this workspace's history that match what is typed.
    suggestions: Vec<ginka_protocol::model::VisitedPage>,
    /// Find in the page, while its field is open.
    finding: Option<Entity<InputState>>,
}

impl BrowserPane {
    /// Create a browser child view attached to this GPUI window.
    pub fn create(window: &mut Window, cx: &mut App) -> Result<Entity<Self>, String> {
        let (sender, receiver) = async_channel::unbounded();
        let ipc_sender = sender.clone();
        let builder = wry::WebViewBuilder::new()
            .with_url("about:blank")
            .with_initialization_script(INSPECT_SCRIPT)
            .with_initialization_script(VISIT_SCRIPT)
            .with_ipc_handler(move |request| {
                let _ = ipc_sender.try_send(BrowserSignal::Inspect(request.body().clone()));
            });
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let window_handle = window
            .window_handle()
            .map_err(|error| format!("browser window handle: {error}"))?;
        let native = builder
            .build_as_child(&window_handle)
            .map_err(|error| format!("browser view: {error}"))?;
        let webview = cx.new(|cx| gpui_wry::WebView::new(native, window, cx));
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.browser.address").to_string())
        });
        let pane = cx.new(|cx| {
            cx.subscribe(
                &address,
                |this: &mut Self, input, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => {
                        let value = input.read(cx).value().to_string();
                        this.navigate(&value, cx);
                    }
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        cx.emit(BrowserEvent::Typed(value));
                    }
                    InputEvent::Blur => {
                        this.suggestions.clear();
                        cx.notify();
                    }
                    _ => {}
                },
            )
            .detach();
            cx.spawn(async move |this, cx| {
                while let Ok(signal) = receiver.recv().await {
                    if this
                        .update(cx, |this, cx| this.receive(signal, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Self {
                webview,
                address,
                inspecting: false,
                visible: true,
                complaint: None,
                suggestions: Vec::new(),
                finding: None,
            }
        });
        Ok(pane)
    }

    fn receive(&mut self, signal: BrowserSignal, cx: &mut Context<Self>) {
        match signal {
            BrowserSignal::Inspect(body) => {
                // A page load reports itself for history; anything else on
                // this channel is an inspection.
                if let Ok(visit) = serde_json::from_str::<VisitEnvelope>(&body)
                    && visit.kind == "ginka-visit"
                {
                    cx.emit(BrowserEvent::Visited {
                        url: visit.url.chars().take(4096).collect(),
                        title: visit.title.map(|title| title.chars().take(512).collect()),
                    });
                    return;
                }
                let parsed = serde_json::from_str::<InspectEnvelope>(&body);
                match parsed {
                    Ok(envelope) if envelope.kind == "ginka-inspect" => {
                        self.inspecting = false;
                        self.complaint = None;
                        cx.emit(BrowserEvent::Inspected(Box::new(sanitize_capture(
                            envelope.capture,
                        ))));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.complaint = Some(format!("browser inspection: {error}").into());
                    }
                }
            }
        }
        cx.notify();
    }

    /// Show or hide the native child view as workspace navigation changes.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.webview.update(cx, |view, _| view.show());
        } else {
            self.inspecting = false;
            self.webview.update(cx, |view, _| view.hide());
        }
        cx.notify();
    }

    /// Offer these pages under the address bar.
    pub fn set_suggestions(
        &mut self,
        pages: Vec<ginka_protocol::model::VisitedPage>,
        cx: &mut Context<Self>,
    ) {
        self.suggestions = pages;
        cx.notify();
    }

    fn navigate(&mut self, address: &str, cx: &mut Context<Self>) {
        let Some(url) = ginka_ui::browser::resolve_address(address) else {
            return;
        };
        self.complaint = None;
        self.suggestions.clear();
        self.webview.update(cx, |view, _| view.load_url(&url));
        cx.notify();
    }

    fn back(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.webview.update(cx, |view, _| {
            let _ = view.back();
        });
    }

    fn forward(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.webview.update(cx, |view, _| {
            let _ = view.evaluate_script("history.forward()");
        });
    }

    fn reload(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.webview.update(cx, |view, _| {
            let _ = view.reload();
        });
    }

    /// Open the find field, or close it.
    fn toggle_find(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding.take().is_some() {
            cx.notify();
            return;
        }
        let field = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("surface.browser.find").to_string())
        });
        field.read(cx).focus_handle(cx).focus(window, cx);
        cx.subscribe(&field, |this: &mut Self, field, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { shift, .. } = event {
                let query = field.read(cx).value().to_string();
                this.find(&query, *shift, cx);
            }
        })
        .detach();
        self.finding = Some(field);
        cx.notify();
    }

    /// Find `query` in the page, forwards or back, wrapping around.
    fn find(&mut self, query: &str, backwards: bool, cx: &mut Context<Self>) {
        let Some(script) = ginka_ui::browser::find_script(query, backwards) else {
            return;
        };
        self.webview.update(cx, |view, _| {
            let _ = view.evaluate_script(&script);
        });
    }

    fn toggle_inspect(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.inspecting = !self.inspecting;
        self.complaint = None;
        let enabled = self.inspecting;
        self.webview.update(cx, |view, _| {
            let _ = view.evaluate_script(&format!(
                "window.__ginkaSetInspect && window.__ginkaSetInspect({enabled})"
            ));
        });
        cx.notify();
    }
}

impl Render for BrowserPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(
                        Button::new("browser-back")
                            .ghost()
                            .compact()
                            .icon(IconName::ArrowLeft)
                            .tooltip(rust_i18n::t!("surface.browser.back").to_string())
                            .on_click(cx.listener(Self::back)),
                    )
                    .child(
                        Button::new("browser-forward")
                            .ghost()
                            .compact()
                            .icon(IconName::ArrowRight)
                            .tooltip(rust_i18n::t!("surface.browser.forward").to_string())
                            .on_click(cx.listener(Self::forward)),
                    )
                    .child(
                        Button::new("browser-reload")
                            .ghost()
                            .compact()
                            .icon(IconName::RotateCw)
                            .tooltip(rust_i18n::t!("surface.browser.reload").to_string())
                            .on_click(cx.listener(Self::reload)),
                    )
                    .child(div().flex_1().child(ginka_ui::field::input(&self.address)))
                    .children(
                        self.finding
                            .as_ref()
                            .map(|field| div().w(px(180.)).child(ginka_ui::field::input(field))),
                    )
                    .child(
                        Button::new("browser-find")
                            .ghost()
                            .compact()
                            .icon(IconName::Search)
                            .tooltip(rust_i18n::t!("surface.browser.find").to_string())
                            .on_click(cx.listener(Self::toggle_find)),
                    )
                    .child(
                        Button::new("browser-inspect")
                            .compact()
                            .when(!self.inspecting, |button| button.ghost())
                            .label(rust_i18n::t!("surface.browser.inspect").to_string())
                            .on_click(cx.listener(Self::toggle_inspect)),
                    ),
            )
            // Between the toolbar and the page rather than over it: the page
            // is a native view drawn above everything GPUI paints.
            .when(!self.suggestions.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .flex_shrink_0()
                        .py_1()
                        .border_b_1()
                        .border_color(tokens.colors().border_subtle)
                        .children(self.suggestions.iter().enumerate().map(|(index, page)| {
                            let url = page.url.clone();
                            h_flex()
                                .id(("browser-suggestion", index))
                                .w_full()
                                .px_3()
                                .py_1()
                                .gap_2()
                                .cursor_pointer()
                                .hover(|this| this.bg(tokens.colors().row_hover()))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.navigate(&url, cx)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .text_color(tokens.colors().text_primary)
                                        .child(
                                            page.title.clone().unwrap_or_else(|| page.url.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .max_w(px(260.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(tokens.colors().text_muted)
                                        .child(page.url.clone()),
                                )
                        })),
                )
            })
            .children(self.complaint.clone().map(|complaint| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(tokens.colors().status_error)
                    .child(complaint)
            }))
            .child(div().flex_1().min_h_0().child(self.webview.clone()))
    }
}

/// Installed in every page: says what loaded, for the address bar's history.
const VISIT_SCRIPT: &str = r#"
(() => {
  if (window.__ginkaVisitInstalled) return;
  window.__ginkaVisitInstalled = true;
  window.addEventListener('load', () => {
    try {
      window.ipc.postMessage(JSON.stringify({
        kind: 'ginka-visit',
        url: String(location.href).slice(0, 4096),
        title: String(document.title || '').slice(0, 512),
      }));
    } catch (_) {}
  });
})();
"#;

/// Installed in every page. It is inert until the native toolbar enables it.
/// The page is untrusted, so the Rust boundary still validates every field.
const INSPECT_SCRIPT: &str = r#"
(() => {
  if (window.__ginkaInspectInstalled) return;
  window.__ginkaInspectInstalled = true;
  let enabled = false;
  let hovered = null;
  const overlay = document.createElement('div');
  Object.assign(overlay.style, {
    position: 'fixed', pointerEvents: 'none', zIndex: '2147483647',
    border: '2px solid #4f9cff', background: 'rgba(79,156,255,.12)',
    display: 'none', boxSizing: 'border-box'
  });
  const mount = () => { if (!overlay.isConnected && document.documentElement) document.documentElement.appendChild(overlay); };
  const show = (element) => {
    mount();
    const rect = element.getBoundingClientRect();
    Object.assign(overlay.style, {
      display: 'block', left: `${rect.left}px`, top: `${rect.top}px`,
      width: `${rect.width}px`, height: `${rect.height}px`
    });
  };
  const selector = (element) => {
    if (element.id) return `#${CSS.escape(element.id)}`;
    const parts = [];
    let node = element;
    while (node && node.nodeType === 1 && parts.length < 8) {
      let part = node.tagName.toLowerCase();
      if (node.classList.length) part += '.' + [...node.classList].slice(0, 3).map(x => CSS.escape(x)).join('.');
      if (node.parentElement) {
        const same = [...node.parentElement.children].filter(x => x.tagName === node.tagName);
        if (same.length > 1) part += `:nth-of-type(${same.indexOf(node) + 1})`;
      }
      parts.unshift(part);
      node = node.parentElement;
    }
    return parts.join(' > ').slice(0, 1024);
  };
  const source = (element) => {
    try {
      for (const key of Object.keys(element)) {
        if (!key.startsWith('__reactFiber$')) continue;
        let fiber = element[key];
        while (fiber) {
          const debug = fiber._debugSource;
          if (debug && debug.fileName) return `${debug.fileName}:${debug.lineNumber || 1}:${debug.columnNumber || 1}`.slice(0, 1024);
          fiber = fiber.return;
        }
      }
    } catch (_) {}
    return null;
  };
  const capture = (element) => {
    const rect = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    const attributes = {};
    for (const attribute of [...(element.attributes || [])].slice(0, 64)) {
      attributes[attribute.name.slice(0, 128)] = attribute.value.slice(0, 2048);
    }
    const nearby = [];
    for (const node of [element.previousElementSibling, element.nextElementSibling, element.parentElement]) {
      const text = node && node.innerText && node.innerText.trim();
      if (text) nearby.push(text.slice(0, 2048));
    }
    return {
      page: { url: location.href.slice(0, 4096), title: document.title.slice(0, 2048), viewport_width: innerWidth, viewport_height: innerHeight },
      element: {
        tag_name: element.tagName.toLowerCase(), selector: selector(element),
        text: (element.innerText || element.textContent || '').trim().slice(0, 2048),
        html: (element.parentElement ? element.parentElement.outerHTML : element.outerHTML).slice(0, 8192),
        source: source(element), attributes,
        accessibility_name: element.getAttribute('aria-label') || element.getAttribute('alt') || null,
        bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
        styles: {
          display: style.display, position: style.position, margin: style.margin,
          padding: style.padding, color: style.color, background: style.backgroundColor,
          border: style.border, border_radius: style.borderRadius, font_family: style.fontFamily,
          font_size: style.fontSize, font_weight: style.fontWeight,
          line_height: style.lineHeight, text_align: style.textAlign
        }
      }, nearby_text: nearby, screenshot_reference: null
    };
  };
  addEventListener('mousemove', event => {
    if (!enabled) return;
    const element = document.elementFromPoint(event.clientX, event.clientY);
    if (!element || element === overlay) return;
    hovered = element; show(element);
  }, true);
  addEventListener('click', event => {
    if (!enabled || !hovered) return;
    event.preventDefault(); event.stopPropagation(); event.stopImmediatePropagation();
    const payload = { kind: 'ginka-inspect', capture: capture(hovered) };
    enabled = false; overlay.style.display = 'none';
    window.ipc.postMessage(JSON.stringify(payload));
  }, true);
  addEventListener('keydown', event => {
    if (enabled && event.key === 'Escape') window.__ginkaSetInspect(false);
  }, true);
  window.__ginkaSetInspect = value => {
    enabled = Boolean(value); hovered = null; mount();
    overlay.style.display = 'none';
    document.documentElement.style.cursor = enabled ? 'crosshair' : '';
  };
})();
"#;
