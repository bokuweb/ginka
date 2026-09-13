//! What a thread sees of a turn, decided from the agent's events.
//!
//! One fold per followed turn. It never touches the platform: it emits
//! [`Outbound`]s, and [`super::deliver`] turns those into transport calls.
//! That split is what makes the throttle on progress edits, the silence
//! token and the reaction swaps testable without a network
//! (`docs/connectors.md` §6).

use super::text::{is_silent, new_request_id};
use super::transport::Glyph;
use ginka_protocol::AgentEvent;
use ginka_protocol::model::SessionState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What kind of question the agent asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// A question, possibly with options.
    Ask,
    /// A plan waiting for approval.
    Plan,
    /// Something the access mode does not allow.
    Permission,
}

/// Something the thread should see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outbound", rename_all = "snake_case")]
pub enum Outbound {
    /// Put a reaction on the message that triggered the turn.
    React { glyph: Glyph },
    /// Take one back off.
    Unreact { glyph: Glyph },
    /// Post, or replace, the one progress message.
    Progress { text: String },
    /// Remove the progress message.
    ClearProgress,
    /// The turn's answer, as the agent's Markdown. Chunking and `mrkdwn`
    /// are the adapter's.
    Reply { markdown: String },
    /// One line: `3 files changed · checkpoint 7 · ginka review comet`.
    /// Filled in by the runner, which can ask the daemon what changed.
    Footer { turn: u32 },
    /// A question with an id the thread answers with.
    Question {
        request_id: String,
        kind: QuestionKind,
        text: String,
        options: Vec<String>,
    },
    /// One line the thread should read: a failure, a cancellation.
    Note { text: String },
}

/// How often the progress message is edited at most, in seconds.
///
/// Slack's `chat.update` budget is about one a second per channel; two is
/// what leaves room for the reply.
pub const PROGRESS_EVERY_SECS: i64 = 2;

/// One followed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnState {
    /// Whether a progress message is wanted at all.
    progress: bool,
    /// Whether it is deleted when the reply lands.
    cleanup: bool,
    /// Whether a progress message has been asked for yet.
    progress_posted: bool,
    /// When it was last edited.
    progress_at: Option<i64>,
    /// The last activity line, whether or not it has been shown.
    activity: Option<String>,
    /// The reply so far.
    reply: String,
    /// Questions the thread has not answered.
    open: Vec<String>,
    /// The short id shown in chat mapped to the transport's interaction id.
    #[serde(default)]
    agent_requests: HashMap<String, String>,
    /// Whether the turn has ended.
    done: bool,
}

impl TurnState {
    /// A fresh turn. `progress` and `cleanup` are the binding's choices.
    pub fn new(progress: bool, cleanup: bool) -> Self {
        Self {
            progress,
            cleanup,
            progress_posted: false,
            progress_at: None,
            activity: None,
            reply: String::new(),
            open: Vec::new(),
            agent_requests: HashMap::new(),
            done: false,
        }
    }

    /// What the thread should see first, before any event.
    pub fn begin(&self) -> Vec<Outbound> {
        vec![Outbound::React {
            glyph: Glyph::Working,
        }]
    }

    /// Whether the turn is over.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The request ids still waiting on an answer.
    pub fn open_requests(&self) -> &[String] {
        &self.open
    }

    /// The thread answered one.
    pub fn close_request(&mut self, request_id: &str) {
        self.open.retain(|open| open != request_id);
        self.agent_requests.remove(request_id);
    }

    /// The transport interaction addressed by a short thread-facing id.
    pub fn agent_request_id(&self, request_id: &str) -> Option<&str> {
        self.agent_requests.get(request_id).map(String::as_str)
    }

    /// Fold one agent event. `now` is unix seconds, for the throttle.
    pub fn on_event(&mut self, event: &AgentEvent, now: i64) -> Vec<Outbound> {
        match event {
            AgentEvent::ToolCall { activity } => {
                self.activity = Some(activity.title.clone());
                self.progress_now(now, false)
            }
            AgentEvent::TextDelta { text } => {
                self.reply.push_str(text);
                Vec::new()
            }
            AgentEvent::AskUser {
                id,
                question,
                options,
            } => self.question(
                id.clone(),
                QuestionKind::Ask,
                question.clone(),
                options.clone(),
            ),
            AgentEvent::PlanProposal { id, plan } => {
                self.question(id.clone(), QuestionKind::Plan, plan.clone(), Vec::new())
            }
            AgentEvent::Permission { id, request } => self.question(
                id.clone(),
                QuestionKind::Permission,
                request.clone(),
                Vec::new(),
            ),
            AgentEvent::TurnEnd { turn } => {
                self.done = true;
                let mut out = Vec::new();
                if self.progress_posted {
                    if self.cleanup {
                        out.push(Outbound::ClearProgress);
                    } else {
                        out.push(Outbound::Progress {
                            text: "Done.".to_string(),
                        });
                    }
                }
                let reply = std::mem::take(&mut self.reply);
                if !reply.trim().is_empty() && !is_silent(&reply) {
                    out.push(Outbound::Reply {
                        markdown: reply.trim().to_string(),
                    });
                }
                out.push(Outbound::Footer { turn: *turn });
                out.push(Outbound::Unreact {
                    glyph: Glyph::Working,
                });
                out.push(Outbound::React { glyph: Glyph::Done });
                out
            }
            _ => Vec::new(),
        }
    }

    /// Fold a session state change. A failure or a cancellation ends the
    /// turn without a `TurnEnd`.
    pub fn on_state(&mut self, state: SessionState, summary: Option<&str>) -> Vec<Outbound> {
        match state {
            SessionState::AwaitingInput => vec![Outbound::React {
                glyph: Glyph::Waiting,
            }],
            SessionState::Failed | SessionState::Cancelled if !self.done => {
                self.done = true;
                let mut out = Vec::new();
                if self.progress_posted {
                    out.push(Outbound::ClearProgress);
                }
                out.push(Outbound::Unreact {
                    glyph: Glyph::Working,
                });
                if state == SessionState::Failed {
                    out.push(Outbound::React {
                        glyph: Glyph::Failed,
                    });
                    out.push(Outbound::Note {
                        text: format!(
                            "The agent failed: {}",
                            summary.unwrap_or("no reason was given")
                        ),
                    });
                } else {
                    out.push(Outbound::React {
                        glyph: Glyph::Stopped,
                    });
                    out.push(Outbound::Note {
                        text: "Stopped.".to_string(),
                    });
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn question(
        &mut self,
        agent_request_id: String,
        kind: QuestionKind,
        text: String,
        options: Vec<String>,
    ) -> Vec<Outbound> {
        let request_id = new_request_id();
        self.open.push(request_id.clone());
        self.agent_requests
            .insert(request_id.clone(), agent_request_id);
        vec![Outbound::Question {
            request_id,
            kind,
            text,
            options,
        }]
    }

    /// Show the current activity, no more often than the throttle allows.
    fn progress_now(&mut self, now: i64, force: bool) -> Vec<Outbound> {
        if !self.progress || self.done {
            return Vec::new();
        }
        let Some(activity) = &self.activity else {
            return Vec::new();
        };
        let due = match self.progress_at {
            None => true,
            Some(at) => now - at >= PROGRESS_EVERY_SECS,
        };
        if !due && !force {
            return Vec::new();
        }
        self.progress_at = Some(now);
        self.progress_posted = true;
        vec![Outbound::Progress {
            text: format!("Working: {activity}"),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::event::ActivityItem;

    fn tool(title: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            activity: ActivityItem::from_tool(None, "Bash", &serde_json::json!({"command": title})),
        }
    }

    #[test]
    fn a_turn_is_acknowledged_then_answered_with_the_reactions_swapped() {
        let mut turn = TurnState::new(true, true);
        assert_eq!(
            turn.begin(),
            vec![Outbound::React {
                glyph: Glyph::Working
            }]
        );
        turn.on_event(
            &AgentEvent::TextDelta {
                text: "Fixed ".into(),
            },
            0,
        );
        turn.on_event(
            &AgentEvent::TextDelta {
                text: "the parser.".into(),
            },
            0,
        );
        let out = turn.on_event(&AgentEvent::TurnEnd { turn: 1 }, 5);
        assert_eq!(
            out,
            vec![
                Outbound::Reply {
                    markdown: "Fixed the parser.".into()
                },
                Outbound::Footer { turn: 1 },
                Outbound::Unreact {
                    glyph: Glyph::Working
                },
                Outbound::React { glyph: Glyph::Done },
            ]
        );
        assert!(turn.is_done());
    }

    #[test]
    fn progress_is_one_message_edited_no_more_than_every_two_seconds() {
        let mut turn = TurnState::new(true, true);
        assert_eq!(
            turn.on_event(&tool("cargo test"), 100),
            vec![Outbound::Progress {
                text: "Working: cargo test".into()
            }]
        );
        assert!(
            turn.on_event(&tool("cargo fmt"), 101).is_empty(),
            "throttled"
        );
        assert_eq!(
            turn.on_event(&tool("cargo clippy"), 102),
            vec![Outbound::Progress {
                text: "Working: cargo clippy".into()
            }]
        );
        // With cleanup on, the progress message goes when the reply lands.
        let out = turn.on_event(&AgentEvent::TurnEnd { turn: 1 }, 103);
        assert_eq!(out[0], Outbound::ClearProgress);
    }

    #[test]
    fn without_cleanup_the_progress_message_collapses_rather_than_going() {
        let mut turn = TurnState::new(true, false);
        turn.on_event(&tool("ls"), 0);
        let out = turn.on_event(&AgentEvent::TurnEnd { turn: 1 }, 9);
        assert_eq!(
            out[0],
            Outbound::Progress {
                text: "Done.".into()
            }
        );
    }

    #[test]
    fn progress_off_means_nothing_until_the_reply() {
        let mut turn = TurnState::new(false, true);
        assert!(turn.on_event(&tool("ls"), 0).is_empty());
        let out = turn.on_event(&AgentEvent::TurnEnd { turn: 1 }, 9);
        assert!(!out.contains(&Outbound::ClearProgress));
    }

    #[test]
    fn a_silent_reply_is_kept_out_of_the_thread() {
        let mut turn = TurnState::new(false, true);
        turn.on_event(
            &AgentEvent::TextDelta {
                text: "[SILENT]".into(),
            },
            0,
        );
        let out = turn.on_event(&AgentEvent::TurnEnd { turn: 2 }, 1);
        assert!(!out.iter().any(|o| matches!(o, Outbound::Reply { .. })));
        assert!(out.contains(&Outbound::Footer { turn: 2 }));
    }

    #[test]
    fn a_question_opens_a_request_the_thread_can_answer() {
        let mut turn = TurnState::new(false, true);
        let out = turn.on_event(
            &AgentEvent::AskUser {
                id: "q1".into(),
                question: "Which file?".into(),
                options: vec!["a.rs".into(), "b.rs".into()],
            },
            0,
        );
        let Outbound::Question {
            request_id,
            kind,
            options,
            ..
        } = &out[0]
        else {
            panic!("expected a question, got {out:?}");
        };
        assert_eq!(*kind, QuestionKind::Ask);
        assert_eq!(turn.agent_request_id(request_id), Some("q1"));
        assert_eq!(options.len(), 2);
        assert_eq!(turn.open_requests(), std::slice::from_ref(request_id));
        turn.close_request(request_id);
        assert!(turn.open_requests().is_empty());
    }

    #[test]
    fn a_failure_ends_the_turn_with_the_reason_and_a_cancel_says_stopped() {
        let mut failed = TurnState::new(true, true);
        failed.on_event(&tool("ls"), 0);
        let out = failed.on_state(SessionState::Failed, Some("not authenticated"));
        assert_eq!(
            out,
            vec![
                Outbound::ClearProgress,
                Outbound::Unreact {
                    glyph: Glyph::Working
                },
                Outbound::React {
                    glyph: Glyph::Failed
                },
                Outbound::Note {
                    text: "The agent failed: not authenticated".into()
                },
            ]
        );
        assert!(failed.is_done());

        let mut stopped = TurnState::new(false, true);
        let out = stopped.on_state(SessionState::Cancelled, None);
        assert!(out.contains(&Outbound::React {
            glyph: Glyph::Stopped
        }));
        // A state change after the turn ended is not a second ending.
        assert!(stopped.on_state(SessionState::Failed, None).is_empty());
    }

    #[test]
    fn waiting_on_the_user_is_shown_without_ending_the_turn() {
        let mut turn = TurnState::new(false, true);
        assert_eq!(
            turn.on_state(SessionState::AwaitingInput, None),
            vec![Outbound::React {
                glyph: Glyph::Waiting
            }]
        );
        assert!(!turn.is_done());
    }
}
