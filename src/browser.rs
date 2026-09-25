//! Native workspace browser and its inspect-to-chat bridge.

#![cfg(any(target_os = "macos", target_os = "windows"))]

use ginka_core::browser::{BrowserCapture, sanitize_capture};
use ginka_ui::Tokens;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_cef::{Webview, WebviewEvent, WebviewOptions};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{IconName, h_flex, v_flex};
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

#[derive(Deserialize)]
struct InspectEnvelope {
    kind: String,
    capture: BrowserCapture,
}

/// Whether the browser engine came up, set once at startup. On macOS it only
/// can inside the bundled app, where Chromium's framework and helpers are.
pub struct BrowserEngine {
    /// Why there is no browser, when there is none.
    pub status: Result<(), String>,
    /// The engine, for what belongs to it rather than to one page — the
    /// cookie store.
    pub runtime: Option<gpui_cef::Runtime>,
}

impl Global for BrowserEngine {}

/// One browser view with toolbar state scoped to the selected workspace.
pub struct BrowserPane {
    webview: Entity<Webview>,
    address: Entity<InputState>,
    inspecting: bool,
    visible: bool,
    /// A page was loading when the view last changed; its finishing is when
    /// the inspector is installed and the visit recorded.
    loading: bool,
    complaint: Option<SharedString>,
    /// Pages from this workspace's history that match what is typed.
    suggestions: Vec<ginka_protocol::model::VisitedPage>,
    /// Find in the page, while its field is open.
    finding: Option<Entity<InputState>>,
    /// Bringing Chrome's cookies over, while that is under way.
    import: CookieImport,
}

/// Where bringing Chrome's cookies over has got to.
#[derive(Clone, PartialEq)]
enum CookieImport {
    Idle,
    /// Which Chrome profile to read.
    Choosing(Vec<ginka_core::chrome_cookies::ChromeProfile>),
    /// Said what will happen; waiting for a yes.
    Confirming(ginka_core::chrome_cookies::ChromeProfile),
    Running,
    /// How it went, in a sentence.
    Done(SharedString),
}

impl BrowserPane {
    /// Create a browser view in this window, or say why there is none.
    pub fn create(window: &mut Window, cx: &mut App) -> Result<Entity<Self>, String> {
        if let Some(BrowserEngine {
            status: Err(reason),
            ..
        }) = cx.try_global::<BrowserEngine>()
        {
            return Err(reason.clone());
        }
        let webview = cx.new(|cx| Webview::new(window, cx, WebviewOptions::url("about:blank")));
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
            cx.subscribe(&webview, |this: &mut Self, _, event: &WebviewEvent, cx| {
                let WebviewEvent::Message(body) = event;
                this.receive(body, cx);
            })
            .detach();
            cx.observe(&webview, |this: &mut Self, webview, cx| {
                let loading = webview.read(cx).is_loading();
                if this.loading && !loading {
                    this.page_loaded(&webview, cx);
                }
                this.loading = loading;
                cx.notify();
            })
            .detach();
            Self {
                webview,
                address,
                inspecting: false,
                visible: true,
                loading: false,
                complaint: None,
                suggestions: Vec::new(),
                finding: None,
                import: CookieImport::Idle,
            }
        });
        Ok(pane)
    }

    /// A page finished loading: give it the inspector, and record the visit.
    ///
    /// The engine has no scripts that run before each page's own, so the
    /// inspector is installed here, and switched back on if it was on.
    fn page_loaded(&mut self, webview: &Entity<Webview>, cx: &mut Context<Self>) {
        let view = webview.read(cx);
        view.eval(&inspect_script());
        if self.inspecting {
            view.eval("window.__ginkaSetInspect && window.__ginkaSetInspect(true)");
        }
        let url = view.url();
        let title = view.title();
        if url.starts_with("http://") || url.starts_with("https://") {
            cx.emit(BrowserEvent::Visited {
                url: url.chars().take(4096).collect(),
                title: (!title.is_empty()).then(|| title.chars().take(512).collect()),
            });
        }
    }

    /// Something the page posted: an inspection, if it is one.
    fn receive(&mut self, body: &str, cx: &mut Context<Self>) {
        match serde_json::from_str::<InspectEnvelope>(body) {
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
        cx.notify();
    }

    /// Show or hide the view as workspace navigation changes.
    ///
    /// On macOS the page is drawn by GPUI like anything else, so hiding is not
    /// drawing it; on Windows it is a native child window that has to be told.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if !visible {
            self.inspecting = false;
        }
        #[cfg(target_os = "windows")]
        self.webview.update(cx, |view, _| view.set_visible(visible));
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
        self.webview.read(cx).load_url(&url);
        cx.notify();
    }

    fn back(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.webview.read(cx).go_back();
    }

    fn forward(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.webview.read(cx).go_forward();
    }

    fn reload(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.webview.read(cx).reload();
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
        self.webview.read(cx).eval(&script);
    }

    fn toggle_inspect(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.inspecting = !self.inspecting;
        self.complaint = None;
        let enabled = self.inspecting;
        let view = self.webview.read(cx);
        // Installed again in case the page predates it; it ignores a second
        // install.
        view.eval(&inspect_script());
        view.eval(&format!(
            "window.__ginkaSetInspect && window.__ginkaSetInspect({enabled})"
        ));
        cx.notify();
    }
}

impl BrowserPane {
    /// Start bringing Chrome's cookies over: pick a profile, or go straight to
    /// confirming when there is one.
    fn begin_import(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.import != CookieImport::Idle && !matches!(self.import, CookieImport::Done(_)) {
            self.import = CookieImport::Idle;
            cx.notify();
            return;
        }
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };
        let mut profiles =
            ginka_core::chrome_cookies::profiles(&ginka_core::chrome_cookies::chrome_dir(&home));
        profiles.retain(|profile| ginka_core::chrome_cookies::store_path(&profile.dir).is_some());
        self.import = match profiles.len() {
            0 => CookieImport::Done(
                rust_i18n::t!("surface.browser.import.none")
                    .to_string()
                    .into(),
            ),
            1 => CookieImport::Confirming(profiles.remove(0)),
            _ => CookieImport::Choosing(profiles),
        };
        cx.notify();
    }

    /// Read the profile's cookies — macOS asks the user before handing over
    /// Chrome's key — and put them in this browser's own store.
    fn run_import(
        &mut self,
        profile: ginka_core::chrome_cookies::ChromeProfile,
        cx: &mut Context<Self>,
    ) {
        self.import = CookieImport::Running;
        cx.notify();
        let read = cx.background_spawn(async move { read_chrome_cookies(&profile) });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            this.update(cx, |this, cx| {
                let outcome = read.and_then(|(name, cookies)| {
                    let runtime = cx
                        .try_global::<BrowserEngine>()
                        .and_then(|engine| engine.runtime.clone())
                        .ok_or_else(|| {
                            rust_i18n::t!("surface.browser.import.no_engine").to_string()
                        })?;
                    let specs: Vec<gpui_cef::CookieSpec> =
                        cookies.iter().map(cookie_spec).collect();
                    let accepted = runtime
                        .set_cookies(&specs)
                        .map_err(|error| error.to_string())?;
                    Ok(rust_i18n::t!(
                        "surface.browser.import.done",
                        count = accepted,
                        profile = name
                    )
                    .to_string())
                });
                this.import = CookieImport::Done(match outcome {
                    Ok(done) => done.into(),
                    Err(error) => rust_i18n::t!("surface.browser.import.failed", error = error)
                        .to_string()
                        .into(),
                });
                // Pages open signed in from here on; the one showing now too.
                this.webview.read(cx).reload();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The strip under the toolbar while cookies are being brought over.
    fn import_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tokens = Tokens::global(cx).clone();
        let row = || {
            h_flex()
                .w_full()
                .flex_shrink_0()
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(tokens.colors().border_subtle)
                .text_sm()
                .text_color(tokens.colors().text_secondary)
        };
        let dismiss = Button::new("import-dismiss")
            .ghost()
            .compact()
            .label(rust_i18n::t!("surface.browser.dismiss").to_string())
            .on_click(cx.listener(|this, _, _, cx| {
                this.import = CookieImport::Idle;
                cx.notify();
            }));
        Some(match &self.import {
            CookieImport::Idle => return None,
            CookieImport::Choosing(profiles) => row()
                .child(div().child(rust_i18n::t!("surface.browser.import.choose").to_string()))
                .children(profiles.iter().enumerate().map(|(index, profile)| {
                    let chosen = profile.clone();
                    Button::new(("import-profile", index))
                        .compact()
                        .label(profile.name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.import = CookieImport::Confirming(chosen.clone());
                            cx.notify();
                        }))
                }))
                .child(div().flex_1())
                .child(dismiss)
                .into_any_element(),
            CookieImport::Confirming(profile) => {
                let chosen = profile.clone();
                row()
                    .child(
                        div().flex_1().min_w_0().child(
                            rust_i18n::t!("surface.browser.import.confirm", profile = profile.name)
                                .to_string(),
                        ),
                    )
                    .child(
                        Button::new("import-go")
                            .compact()
                            .label(rust_i18n::t!("surface.browser.import.go").to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_import(chosen.clone(), cx)
                            })),
                    )
                    .child(dismiss)
                    .into_any_element()
            }
            CookieImport::Running => row()
                .child(rust_i18n::t!("surface.browser.import.running").to_string())
                .into_any_element(),
            CookieImport::Done(message) => row()
                .child(div().flex_1().min_w_0().child(message.clone()))
                .child(dismiss)
                .into_any_element(),
        })
    }
}

/// Chrome's key from the Keychain — macOS asks the user first — then the
/// profile's cookies, decrypted. Blocking; run off the main thread.
fn read_chrome_cookies(
    profile: &ginka_core::chrome_cookies::ChromeProfile,
) -> Result<(String, Vec<ginka_core::chrome_cookies::ChromeCookie>), String> {
    use ginka_core::chrome_cookies::{derive_key, read_cookies, store_path};

    let store = store_path(&profile.dir)
        .ok_or_else(|| rust_i18n::t!("surface.browser.import.none").to_string())?;
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-w", "-s", "Chrome Safe Storage"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(rust_i18n::t!("surface.browser.import.declined").to_string());
    }
    let password = String::from_utf8_lossy(&output.stdout);
    let key = derive_key(password.trim_end_matches('\n').as_bytes());
    let cookies = read_cookies(&store, &key).map_err(|error| error.to_string())?;
    Ok((profile.name.clone(), cookies))
}

/// A Chrome cookie as the engine's store takes it.
fn cookie_spec(cookie: &ginka_core::chrome_cookies::ChromeCookie) -> gpui_cef::CookieSpec {
    use ginka_core::chrome_cookies::{SameSite, cookie_domain, cookie_url};
    gpui_cef::CookieSpec {
        url: cookie_url(cookie),
        name: cookie.name.clone(),
        value: cookie.value.clone(),
        domain: cookie_domain(cookie),
        path: cookie.path.clone(),
        secure: cookie.secure,
        http_only: cookie.http_only,
        expires: cookie.expires,
        same_site: match cookie.same_site {
            SameSite::Unspecified => gpui_cef::SameSite::Unspecified,
            SameSite::None => gpui_cef::SameSite::None,
            SameSite::Lax => gpui_cef::SameSite::Lax,
            SameSite::Strict => gpui_cef::SameSite::Strict,
        },
    }
}

/// The inspector, posting what it captures through the engine's message
/// channel (`gpui_cef::post_message_script`'s console route).
fn inspect_script() -> String {
    format!(
        "window.__ginkaPost = (message) => console.debug({prefix:?} + message);\n{INSPECT_SCRIPT}",
        prefix = gpui_cef::MESSAGE_PREFIX,
    )
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
                    .when(cfg!(target_os = "macos"), |this| {
                        this.child(
                            Button::new("browser-import")
                                .ghost()
                                .compact()
                                .label(rust_i18n::t!("surface.browser.import").to_string())
                                .tooltip(rust_i18n::t!("surface.browser.import.tip").to_string())
                                .on_click(cx.listener(Self::begin_import)),
                        )
                    })
                    .child(
                        Button::new("browser-inspect")
                            .compact()
                            .when(!self.inspecting, |button| button.ghost())
                            .label(rust_i18n::t!("surface.browser.inspect").to_string())
                            .on_click(cx.listener(Self::toggle_inspect)),
                    ),
            )
            .children(self.import_strip(cx))
            // Between the toolbar and the page rather than over it: on
            // Windows the page is a native view drawn above everything GPUI
            // paints.
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
    window.__ginkaPost(JSON.stringify(payload));
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
