//! The far-right CLI-backed conversation pane (`docs/ui.md` §3.7).
//!
//! The detail view supplies GitHub context and this pane owns the lifetime of
//! the conversation. Each turn runs on GPUI's background executor through an
//! installed CLI's structured-output mode; the returned session id is passed
//! back on the next turn, so the surface is a chat rather than a sequence of
//! unrelated prompts.

use crate::store::Store;
use e1_ui::Tokens;
use e1_ui::agents::{self, Ask, Kind, Tuning};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::bubble::{Bubble, BubbleVariant};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::message::{Message, MessageAlignment, MessageContent};
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_component::text::TextView;
use gpui_component::{Icon, IconName, StyledExt as _, h_flex, v_flex};

/// What the pane asks its host shell to do.
pub enum AgentPaneEvent {
    /// Close the far-right column without discarding its conversation.
    Close,
    /// Persist the selected CLI.
    AgentChosen(Kind),
    /// Persist model and effort choices for a CLI.
    Tuned(Kind, Tuning),
}

impl EventEmitter<AgentPaneEvent> for AgentPane {}

#[derive(Clone)]
struct ChatMessage {
    id: usize,
    sent: bool,
    failed: bool,
    body: String,
}

/// A conversation about the item, file, diff, or log beside it.
pub struct AgentPane {
    store: Entity<Store>,
    context: Option<Ask>,
    messages: Vec<ChatMessage>,
    session: Option<String>,
    composer: Entity<TextareaState>,
    scroller: Entity<MessageScrollerState>,
    running: bool,
    agents_open: bool,
    focus_composer: bool,
    next_id: usize,
    task: Option<Task<()>>,
}

impl AgentPane {
    /// Create an empty pane; it receives context when the reader presses Ask.
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(rust_i18n::t!("ask.chat.placeholder").to_string())
                .auto_grow(2, 8)
        });
        cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: true,
                        ..
                    }
                ) {
                    this.send(window, cx);
                }
            },
        )
        .detach();
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        Self {
            store,
            context: None,
            messages: Vec::new(),
            session: None,
            composer,
            scroller,
            running: false,
            agents_open: false,
            focus_composer: false,
            next_id: 0,
            task: None,
        }
    }

    /// Open on some GitHub context. Reopening the same subject resumes the
    /// visible conversation; moving to another subject starts a fresh one.
    pub fn open(&mut self, ask: Ask, cx: &mut Context<Self>) {
        let changed = self.context.as_ref().is_none_or(|current| {
            current.repo != ask.repo
                || current.subject != ask.subject
                || current.source != ask.source
        });
        if changed {
            self.messages.clear();
            self.session = None;
            self.running = false;
            self.task = None;
            self.scroller.update(cx, |state, cx| state.reset(0, cx));
        }
        self.context = Some(ask);
        self.agents_open = false;
        self.focus_composer = true;
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let question = self.composer.read(cx).value().trim().to_string();
        if question.is_empty() {
            return;
        }
        let Some(agent) = self.store.read(cx).chosen_agent().cloned() else {
            return;
        };
        let Some(mut ask) = self.context.clone() else {
            return;
        };
        let prompt = if self.session.is_none() {
            ask.question = question.clone();
            ask.prompt()
        } else {
            question.clone()
        };
        let tuning = self.store.read(cx).tuning(agent.kind);
        let workdir = ask
            .repo
            .as_ref()
            .and_then(|repo| agents::checkout_in(repo, &agents::checkout_roots()));
        let session = self.session.clone();
        let demo = std::env::var_os("E1_DEMO").is_some();
        let demo_reply = rust_i18n::t!("ask.chat.demo_reply").to_string();

        self.messages.push(ChatMessage {
            id: self.next_id,
            sent: true,
            failed: false,
            body: question,
        });
        self.next_id += 1;
        self.scroller
            .update(cx, |state, cx| _ = state.append(1, cx));
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        self.running = true;
        self.agents_open = false;
        cx.emit(AgentPaneEvent::AgentChosen(agent.kind));
        cx.notify();

        self.task = Some(cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move {
                    if demo {
                        Ok(agents::ChatReply {
                            text: demo_reply,
                            session: Some("e1-demo-session".to_string()),
                        })
                    } else {
                        agents::chat(
                            &agent,
                            &prompt,
                            &tuning,
                            session.as_deref(),
                            workdir.as_deref(),
                        )
                    }
                })
                .await;
            _ = this.update(cx, |this, cx| {
                let (body, failed) = match answer {
                    Ok(reply) => {
                        this.session = reply.session.or(this.session.take());
                        (reply.text, false)
                    }
                    Err(error) => (error.to_string(), true),
                };
                this.messages.push(ChatMessage {
                    id: this.next_id,
                    sent: false,
                    failed,
                    body,
                });
                this.next_id += 1;
                this.running = false;
                this.task = None;
                this.scroller
                    .update(cx, |state, cx| _ = state.append(1, cx));
                cx.notify();
            });
        }));
    }

    fn choose_agent(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.choose_agent(kind, cx));
        self.session = None;
        self.agents_open = false;
        cx.emit(AgentPaneEvent::AgentChosen(kind));
        cx.notify();
    }

    fn cycle_model(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.store.read(cx).chosen_agent().map(|agent| agent.kind) else {
            return;
        };
        let current = self.store.read(cx).tuning(kind);
        let choices = kind.models();
        if choices.is_empty() {
            return;
        }
        let next = current
            .model()
            .and_then(|model| choices.iter().position(|choice| choice.id == model))
            .and_then(|index| choices.get(index + 1).map(|choice| choice.id.to_string()));
        let mut tuning = current;
        tuning.model = next;
        self.store
            .update(cx, |store, cx| store.tune(kind, tuning.clone(), cx));
        self.session = None;
        cx.emit(AgentPaneEvent::Tuned(kind, tuning));
        cx.notify();
    }

    fn cycle_effort(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.store.read(cx).chosen_agent().map(|agent| agent.kind) else {
            return;
        };
        let current = self.store.read(cx).tuning(kind);
        let choices = kind.efforts();
        if choices.is_empty() {
            return;
        }
        let next = current
            .effort()
            .and_then(|effort| choices.iter().position(|choice| choice.id == effort))
            .and_then(|index| choices.get(index + 1).map(|choice| choice.id.to_string()));
        let mut tuning = current;
        tuning.effort = next;
        self.store
            .update(cx, |store, cx| store.tune(kind, tuning.clone(), cx));
        self.session = None;
        cx.emit(AgentPaneEvent::Tuned(kind, tuning));
        cx.notify();
    }

    fn message_row(message: ChatMessage) -> AnyElement {
        let alignment = if message.sent {
            MessageAlignment::End
        } else {
            MessageAlignment::Start
        };
        let variant = if message.failed {
            BubbleVariant::Destructive
        } else if message.sent {
            BubbleVariant::Muted
        } else {
            BubbleVariant::Ghost
        };
        let body: AnyElement = if message.sent || message.failed {
            div()
                .whitespace_normal()
                .child(message.body)
                .into_any_element()
        } else {
            TextView::markdown(("agent-message", message.id), message.body).into_any_element()
        };
        div()
            .id(("agent-message-row", message.id))
            .w_full()
            .child(Message::new().alignment(alignment).content(
                MessageContent::new().bubble(Bubble::new().with_variant(variant).child(body)),
            ))
            .into_any_element()
    }

    fn choice_button(
        &self,
        id: &'static str,
        label: String,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let tokens = Tokens::global(cx).clone();
        h_flex()
            .id(id)
            .max_w(px(170.))
            .min_w_0()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .rounded(px(tokens.radius.control()))
            .border_1()
            .border_color(tokens.colors().border_subtle)
            .cursor_pointer()
            .hover(|this| this.bg(tokens.colors().row_hover()))
            .child(div().min_w_0().truncate().child(label))
            .child(Icon::new(IconName::ChevronDown).size_3())
            .on_click(cx.listener(move |this, _, _, cx| click(this, cx)))
    }
}

impl Render for AgentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_composer {
            self.focus_composer = false;
            let composer = self.composer.clone();
            window.defer(cx, move |window, cx| {
                composer.read(cx).focus_handle(cx).focus(window, cx);
            });
        }
        let tokens = Tokens::global(cx).clone();
        let title = self
            .context
            .as_ref()
            .map(|ask| ask.subject.clone())
            .unwrap_or_else(|| rust_i18n::t!("ask.title").to_string());
        let messages = cx.entity();
        let scroller = MessageScroller::new(
            "agent-messages",
            self.scroller.clone(),
            move |index, _, cx| {
                messages
                    .read(cx)
                    .messages
                    .get(index)
                    .cloned()
                    .map(AgentPane::message_row)
                    .unwrap_or_else(|| div().into_any_element())
            },
        )
        .with_jump_button_label(rust_i18n::t!("ask.chat.latest").to_string())
        .size_full();
        let agents = self.store.read(cx).agents().to_vec();
        let agents: Vec<_> = agents
            .into_iter()
            .filter(|agent| agent.kind.supports_chat())
            .collect();
        let chosen = self.store.read(cx).chosen_agent().cloned();
        let tuning = chosen
            .as_ref()
            .map(|agent| self.store.read(cx).tuning(agent.kind))
            .unwrap_or_default();
        let agent_rows: Vec<AnyElement> = if self.agents_open {
            agents
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, agent)| {
                    let kind = agent.kind;
                    h_flex()
                        .id(("agent-choice", index))
                        .w_full()
                        .px_2()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .rounded(px(tokens.radius.row))
                        .cursor_pointer()
                        .hover(|this| this.bg(tokens.colors().row_hover()))
                        .child(Icon::new(IconName::SquareTerminal).size_3p5())
                        .child(agent.label())
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_agent(kind, cx)))
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        let agent_label = chosen
            .as_ref()
            .map(|agent| agent.label().to_string())
            .unwrap_or_else(|| rust_i18n::t!("ask.chat.no_agent").to_string());
        let model = tuning
            .model()
            .map(str::to_string)
            .unwrap_or_else(|| rust_i18n::t!("ask.default").to_string());
        let effort = tuning
            .effort()
            .map(str::to_string)
            .unwrap_or_else(|| rust_i18n::t!("ask.default").to_string());

        v_flex()
            .size_full()
            .bg(tokens.colors().bg_surface)
            .child(
                h_flex()
                    .h(e1_ui::HEADER_HEIGHT)
                    .w_full()
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(tokens.colors().border_subtle)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_medium()
                            .child(format!("{} · {title}", rust_i18n::t!("ask.chat.title"))),
                    )
                    .children(self.running.then(|| {
                        gpui_component::spinner::Spinner::new()
                            .icon(IconName::LoaderCircle)
                            .into_any_element()
                    }))
                    .child(
                        div()
                            .id("close-agent-pane")
                            .p_1()
                            .rounded(px(tokens.radius.control()))
                            .cursor_pointer()
                            .hover(|this| this.bg(tokens.colors().row_hover()))
                            .child(Icon::new(IconName::PanelRightClose).size_4())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(AgentPaneEvent::Close))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(self.messages.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .size_full()
                                .gap_3()
                                .items_center()
                                .justify_center()
                                .text_color(tokens.colors().text_muted)
                                .child(Icon::new(IconName::Bot).size_8())
                                .child(rust_i18n::t!("ask.chat.empty").to_string()),
                        )
                    })
                    .when(!self.messages.is_empty(), |this| this.child(scroller)),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .m_2()
                    .p_2()
                    .gap_2()
                    .rounded(px(tokens.radius.control() + 2.))
                    .border_1()
                    .border_color(tokens.colors().border_strong)
                    .bg(tokens.colors().popover())
                    .children(agent_rows)
                    .child(Textarea::new(&self.composer))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .items_center()
                            .child(self.choice_button(
                                "agent-picker",
                                agent_label,
                                cx,
                                |this, cx| {
                                    this.agents_open = !this.agents_open;
                                    cx.notify();
                                },
                            ))
                            .children(chosen.as_ref().and_then(|agent| {
                                (!agent.kind.models().is_empty()).then(|| {
                                    self.choice_button("model-picker", model, cx, |this, cx| {
                                        this.cycle_model(cx)
                                    })
                                })
                            }))
                            .children(chosen.as_ref().and_then(|agent| {
                                (!agent.kind.efforts().is_empty()).then(|| {
                                    self.choice_button("effort-picker", effort, cx, |this, cx| {
                                        this.cycle_effort(cx)
                                    })
                                })
                            }))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("send-agent-message")
                                    .p_2()
                                    .rounded(px(tokens.radius.control()))
                                    .bg(tokens.colors().accent)
                                    .text_color(tokens.colors().bg_window)
                                    .cursor_pointer()
                                    .child(Icon::new(IconName::ArrowUp).size_4())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.send(window, cx)),
                                    ),
                            ),
                    ),
            )
    }
}
