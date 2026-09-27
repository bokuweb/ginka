//! The sign-in screen: GitHub's device flow, drawn.
//!
//! The reader presses one button, is shown a short code, types it into a
//! page in their own browser, and the window signs itself in when GitHub
//! says so. No password passes through this process, and the token that
//! comes back goes to the keychain, not to a file.

use e1_github::auth::device::{self, DeviceCode, Poll};
use e1_github::auth::{self, Keychain, Token};
use e1_ui::Tokens;
use e1_ui::fetch::describe;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};
use std::time::Duration;

/// Emitted when a token arrives.
pub enum SignInEvent {
    /// Signed in with this token.
    SignedIn(Token),
}

impl EventEmitter<SignInEvent> for SignIn {}

/// Where the flow is.
enum State {
    /// The button.
    Idle,
    /// Asked GitHub for a code, no answer yet.
    Starting,
    /// The code is on screen and the app is polling.
    Waiting {
        code: DeviceCode,
        /// The code was copied to the clipboard; the button says so.
        copied: bool,
    },
    /// Something went wrong, in the reader's words.
    Failed(String),
}

/// The sign-in screen.
pub struct SignIn {
    state: State,
    /// Which attempt is running. A poll from an earlier attempt that lands
    /// after a retry must not overwrite the newer one's state.
    attempt: u64,
}

impl SignIn {
    /// The screen, showing the button.
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            attempt: 0,
        }
    }

    fn fail(&mut self, attempt: u64, message: String, cx: &mut Context<Self>) {
        if attempt == self.attempt {
            self.state = State::Failed(message);
            cx.notify();
        }
    }

    /// Ask GitHub for a code and poll until it answers.
    fn start(&mut self, cx: &mut Context<Self>) {
        let client_id = auth::client_id();
        self.attempt += 1;
        let attempt = self.attempt;
        self.state = State::Starting;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let id = client_id.clone();
            let started = cx.background_spawn(async move { device::start(&id) }).await;
            let code = match started {
                Ok(code) => code,
                Err(error) => {
                    this.update(cx, |this, cx| this.fail(attempt, describe(&error), cx))
                        .ok();
                    return;
                }
            };
            let mut interval = code.interval();
            let device_code = code.device_code.clone();
            let shown = this.update(cx, |this, cx| {
                if attempt == this.attempt {
                    this.state = State::Waiting {
                        code,
                        copied: false,
                    };
                    cx.notify();
                }
            });
            if shown.is_err() {
                return;
            }
            loop {
                cx.background_executor().timer(interval).await;
                // A retry started another attempt; this one stops polling.
                let superseded = this
                    .update(cx, |this, _| attempt != this.attempt)
                    .unwrap_or(true);
                if superseded {
                    return;
                }
                let (id, device_code) = (client_id.clone(), device_code.clone());
                let polled = cx
                    .background_spawn(async move { device::poll(&id, &device_code) })
                    .await;
                let outcome: Result<Token, String> = match polled {
                    Ok(Poll::Pending) => continue,
                    Ok(Poll::SlowDown) => {
                        interval += Duration::from_secs(5);
                        continue;
                    }
                    Ok(Poll::Granted(token)) => Ok(token),
                    Ok(Poll::Denied) => Err(rust_i18n::t!("signin.denied").to_string()),
                    Ok(Poll::Expired) => Err(rust_i18n::t!("signin.expired").to_string()),
                    Err(error) => Err(describe(&error)),
                };
                match outcome {
                    Ok(token) => {
                        let kept = token.clone();
                        if let Err(error) = cx
                            .background_spawn(async move { Keychain::store(&kept) })
                            .await
                        {
                            // Signed in for this session all the same; the
                            // reader signs in again next launch.
                            tracing::warn!(%error, "could not keep the token in the keychain");
                        }
                        this.update(cx, |this, cx| {
                            this.state = State::Idle;
                            cx.emit(SignInEvent::SignedIn(token));
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(message) => {
                        this.update(cx, |this, cx| this.fail(attempt, message, cx))
                            .ok();
                    }
                }
                return;
            }
        })
        .detach();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let State::Waiting { code, copied } = &mut self.state {
            cx.write_to_clipboard(ClipboardItem::new_string(code.user_code.clone()));
            *copied = true;
            cx.notify();
        }
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        if let State::Waiting { code, .. } = &self.state {
            let uri = code.verification_uri.clone();
            self.copy(cx);
            cx.open_url(&uri);
        }
    }

    /// The one filled button on the screen.
    fn primary(
        &self,
        id: &'static str,
        label: String,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        div()
            .id(id)
            .px_4()
            .py_2()
            .rounded(px(tokens.radius.control()))
            .bg(tokens.colors().accent)
            .hover(|this| this.opacity(0.85))
            .cursor_pointer()
            .text_size(px(13.))
            .font_medium()
            .text_color(tokens.colors().bg_window)
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .into_any_element()
    }

    /// A quiet button beside the primary one.
    fn secondary(
        &self,
        id: &'static str,
        label: String,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let tokens = Tokens::global(cx);
        div()
            .id(id)
            .px_4()
            .py_2()
            .rounded(px(tokens.radius.control()))
            .bg(tokens.colors().bg_surface)
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(tokens.colors().text_primary)
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .into_any_element()
    }
}

impl Default for SignIn {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for SignIn {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::global(cx).clone();
        let mono = gpui_component::Theme::global(cx).mono_font_family.clone();
        let muted = tokens.colors().text_muted;

        let body: AnyElement = match &self.state {
            State::Idle => v_flex()
                .items_center()
                .gap_3()
                .child(self.primary(
                    "sign-in",
                    rust_i18n::t!("signin.button").to_string(),
                    cx,
                    |this, cx| this.start(cx),
                ))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(muted)
                        .text_center()
                        .child(rust_i18n::t!("signin.hint").to_string()),
                )
                .into_any_element(),
            State::Starting => div()
                .text_size(px(13.))
                .text_color(muted)
                .child(rust_i18n::t!("signin.starting").to_string())
                .into_any_element(),
            State::Waiting { code, copied } => v_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(muted)
                        .child(rust_i18n::t!("signin.enter_code").to_string()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(tokens.colors().accent)
                        .child(code.verification_uri.clone()),
                )
                .child(
                    div()
                        .px_5()
                        .py_3()
                        .rounded(px(tokens.radius.panel))
                        .bg(tokens.colors().bg_surface)
                        .border_1()
                        .border_color(tokens.colors().border_strong)
                        .font_family(mono)
                        .text_2xl()
                        .text_color(tokens.colors().text_primary)
                        .child(code.user_code.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(self.primary(
                            "open-device",
                            rust_i18n::t!("signin.open").to_string(),
                            cx,
                            |this, cx| this.open(cx),
                        ))
                        .child(
                            self.secondary(
                                "copy-code",
                                rust_i18n::t!(if *copied {
                                    "signin.copied"
                                } else {
                                    "signin.copy"
                                })
                                .to_string(),
                                cx,
                                |this, cx| this.copy(cx),
                            ),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .text_size(px(11.5))
                        .text_color(muted)
                        .child({
                            use gpui_component::Sizable as _;
                            gpui_component::spinner::Spinner::new()
                                .icon(IconName::LoaderCircle)
                                .with_size(px(12.))
                                .color(muted)
                        })
                        .child(rust_i18n::t!("signin.waiting").to_string()),
                )
                .into_any_element(),
            State::Failed(message) => v_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(tokens.colors().status_error)
                        .text_center()
                        .child(message.clone()),
                )
                .child(self.secondary(
                    "retry",
                    rust_i18n::t!("signin.retry").to_string(),
                    cx,
                    |this, cx| this.start(cx),
                ))
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .px_8()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(420.))
                    .items_center()
                    .gap_5()
                    .child(
                        Icon::empty()
                            .path(e1_ui::assets::icon::E1)
                            .size(px(56.))
                            .text_color(tokens.logo()),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_medium()
                            .text_color(tokens.colors().text_primary)
                            .child(rust_i18n::t!("signin.title").to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(tokens.colors().text_secondary)
                            .text_center()
                            .child(rust_i18n::t!("signin.lead").to_string()),
                    )
                    .child(body),
            )
            .when(false, |this| this)
    }
}
