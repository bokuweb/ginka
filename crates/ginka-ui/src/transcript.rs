//! Folding an event stream into something a window can draw.
//!
//! The daemon stores and streams normalized events — a text delta, a tool
//! call, a turn boundary — because that is what survives being replayed. A
//! reader wants paragraphs and tool cards. This is where one becomes the
//! other, and it is here rather than in the views because it is the part with
//! rules worth testing.
//!
//! Entries arrive twice by design: once as the page fetched when a session is
//! opened, and again as pushes from the daemon. Every entry carries its
//! position, so applying one is idempotent and a missing position is
//! detectable rather than silently swallowed.

use ginka_protocol::model::{SessionState, TranscriptEntry, TranscriptPayload};
use ginka_protocol::{AgentEvent, Usage};

/// One drawable piece of a transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Something the user sent.
    User { text: String },
    /// Assistant prose, with its deltas already folded together.
    Assistant { text: String },
    /// The agent's reasoning, where the vendor exposes it.
    Reasoning { text: String },
    /// A tool call and, once it arrives, its result.
    Tool {
        /// Correlates the call with its result.
        id: String,
        name: String,
        input: String,
        /// `None` while the tool is still running.
        output: Option<String>,
        is_error: bool,
    },
    /// The agent is blocked on the user.
    Question {
        question: String,
        options: Vec<String>,
    },
    /// A plan the agent wants approved before acting.
    Plan { plan: String },
    /// A turn boundary. A checkpoint was taken here.
    TurnEnd { turn: u32 },
    /// How the session ended.
    Outcome {
        state: SessionState,
        summary: Option<String>,
    },
}

/// What applying an entry did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// It was folded in.
    Added,
    /// It had already been applied; the same event arrives by push and in the
    /// fetched page, and folding it twice would double the text.
    AlreadySeen,
    /// Its position is beyond the next one expected, so something in between
    /// was missed. The caller has to re-read rather than carry on: a
    /// transcript with a hole in it is worse than one that is refetched.
    Gap { expected: u64 },
}

/// A session's transcript, folded.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    blocks: Vec<Block>,
    cursor: u64,
    usage: Usage,
}

impl Transcript {
    /// An empty transcript, positioned before the first entry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The highest position folded in. This is the cursor to page from.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// What the window draws.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The session's accounting so far, as the last `Usage` event reported it.
    ///
    /// Kept off the block list: it belongs in the context bar, not in the
    /// middle of the conversation.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Fold one entry in.
    pub fn apply(&mut self, entry: &TranscriptEntry) -> Applied {
        if entry.seq <= self.cursor {
            return Applied::AlreadySeen;
        }
        if entry.seq > self.cursor + 1 {
            return Applied::Gap {
                expected: self.cursor + 1,
            };
        }
        self.cursor = entry.seq;

        match &entry.payload {
            TranscriptPayload::User { text } => {
                self.blocks.push(Block::User { text: text.clone() })
            }
            TranscriptPayload::Agent { event } => self.fold(event),
        }
        Applied::Added
    }

    /// Fold a page in, stopping at the first gap.
    ///
    /// Returns what the last entry did, so a caller can tell a page that
    /// applied cleanly from one that revealed a hole.
    pub fn extend<'a>(
        &mut self,
        entries: impl IntoIterator<Item = &'a TranscriptEntry>,
    ) -> Applied {
        let mut last = Applied::AlreadySeen;
        for entry in entries {
            last = self.apply(entry);
            if matches!(last, Applied::Gap { .. }) {
                return last;
            }
        }
        last
    }

    fn fold(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta { text } => self.append_text(text),
            AgentEvent::Reasoning { text } => self.append_reasoning(text),
            AgentEvent::ToolCall { id, name, input } => self.blocks.push(Block::Tool {
                id: id.clone(),
                name: name.clone(),
                input: render_input(input),
                output: None,
                is_error: false,
            }),
            AgentEvent::ToolResult {
                id,
                output,
                is_error,
            } => self.attach_result(id, output, *is_error),
            AgentEvent::AskUser {
                question, options, ..
            } => self.blocks.push(Block::Question {
                question: question.clone(),
                options: options.clone(),
            }),
            AgentEvent::PlanProposal { plan, .. } => {
                self.blocks.push(Block::Plan { plan: plan.clone() })
            }
            // Accounting belongs in the context bar, not in the conversation.
            AgentEvent::Usage { usage } => self.usage = *usage,
            AgentEvent::TurnEnd { turn } => self.blocks.push(Block::TurnEnd { turn: *turn }),
            AgentEvent::SessionResult { state, summary } => self.blocks.push(Block::Outcome {
                state: *state,
                summary: summary.clone(),
            }),
        }
    }

    /// Grow the open assistant paragraph, or start one.
    fn append_text(&mut self, text: &str) {
        match self.blocks.last_mut() {
            Some(Block::Assistant { text: existing }) => existing.push_str(text),
            _ => self.blocks.push(Block::Assistant {
                text: text.to_string(),
            }),
        }
    }

    fn append_reasoning(&mut self, text: &str) {
        match self.blocks.last_mut() {
            Some(Block::Reasoning { text: existing }) => existing.push_str(text),
            _ => self.blocks.push(Block::Reasoning {
                text: text.to_string(),
            }),
        }
    }

    /// Attach a result to the call that asked for it.
    ///
    /// Searched from the end because a result belongs to the most recent call
    /// with that id, and an id a driver had to synthesise may repeat across a
    /// long session. A result with no call is still shown: losing output
    /// because the call was in a page we have not fetched would be worse than
    /// an unattached card.
    fn attach_result(&mut self, id: &str, output: &str, is_error: bool) {
        let matching = self.blocks.iter_mut().rev().find(
            |block| matches!(block, Block::Tool { id: call, output: None, .. } if call == id),
        );
        match matching {
            Some(Block::Tool {
                output: slot,
                is_error: failed,
                ..
            }) => {
                *slot = Some(output.to_string());
                *failed = is_error;
            }
            _ => self.blocks.push(Block::Tool {
                id: id.to_string(),
                name: String::new(),
                input: String::new(),
                output: Some(output.to_string()),
                is_error,
            }),
        }
    }
}

/// The first `limit` lines of `text`, with a note when there were more.
///
/// A tool that printed a megabyte must not push the conversation off the
/// screen. The whole output is still in the transcript, and `ginka session log`
/// prints it.
pub fn head_of(text: &str, limit: usize) -> String {
    let mut lines = text.lines();
    let head: Vec<&str> = lines.by_ref().take(limit).collect();
    let rest = lines.count();
    if rest == 0 {
        return head.join("\n");
    }
    format!("{}\n… {rest} more lines", head.join("\n"))
}

/// A tool's arguments as one line.
///
/// Objects are unwrapped to `key=value` pairs: `{"file_path": "a.rs"}` reads as
/// noise, `file_path=a.rs` reads as what the agent did.
fn render_input(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| match value {
                serde_json::Value::String(text) => format!("{key}={text}"),
                other => format!("{key}={other}"),
            })
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(seq: u64, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            seq,
            at: 0,
            payload: TranscriptPayload::User { text: text.into() },
        }
    }

    fn agent(seq: u64, event: AgentEvent) -> TranscriptEntry {
        TranscriptEntry {
            seq,
            at: 0,
            payload: TranscriptPayload::Agent { event },
        }
    }

    fn text(seq: u64, text: &str) -> TranscriptEntry {
        agent(seq, AgentEvent::TextDelta { text: text.into() })
    }

    #[test]
    fn deltas_fold_into_one_paragraph() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            user(1, "hello"),
            text(2, "Hel"),
            text(3, "lo "),
            text(4, "back"),
        ]);
        assert_eq!(
            transcript.blocks(),
            &[
                Block::User {
                    text: "hello".into()
                },
                Block::Assistant {
                    text: "Hello back".into()
                },
            ]
        );
        assert_eq!(transcript.cursor(), 4);
    }

    #[test]
    fn a_new_prompt_closes_the_paragraph_before_it() {
        let mut transcript = Transcript::new();
        transcript.extend(&[text(1, "done"), user(2, "again"), text(3, "done")]);
        assert_eq!(transcript.blocks().len(), 3);
        assert!(matches!(transcript.blocks()[2], Block::Assistant { .. }));
    }

    #[test]
    fn reasoning_and_prose_are_separate_blocks() {
        // They are drawn differently; folding them together would put the
        // agent's private thinking into what it said.
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::Reasoning {
                    text: "weighing ".into(),
                },
            ),
            agent(
                2,
                AgentEvent::Reasoning {
                    text: "it up".into(),
                },
            ),
            text(3, "Here is why."),
        ]);
        assert_eq!(
            transcript.blocks(),
            &[
                Block::Reasoning {
                    text: "weighing it up".into()
                },
                Block::Assistant {
                    text: "Here is why.".into()
                },
            ]
        );
    }

    #[test]
    fn a_tool_result_lands_on_the_call_that_asked_for_it() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::ToolCall {
                    id: "t1".into(),
                    name: "Read".into(),
                    input: json!({ "file_path": "a.rs" }),
                },
            ),
            text(2, "reading"),
            agent(
                3,
                AgentEvent::ToolResult {
                    id: "t1".into(),
                    output: "fn main() {}".into(),
                    is_error: false,
                },
            ),
        ]);
        assert_eq!(
            transcript.blocks()[0],
            Block::Tool {
                id: "t1".into(),
                name: "Read".into(),
                input: "file_path=a.rs".into(),
                output: Some("fn main() {}".into()),
                is_error: false,
            }
        );
    }

    #[test]
    fn a_result_with_no_call_is_still_shown() {
        // The call may be in a page that has not been fetched; dropping the
        // output would lose it for good.
        let mut transcript = Transcript::new();
        transcript.extend(&[agent(
            1,
            AgentEvent::ToolResult {
                id: "orphan".into(),
                output: "surprise".into(),
                is_error: true,
            },
        )]);
        assert!(matches!(
            &transcript.blocks()[0],
            Block::Tool {
                output: Some(_),
                is_error: true,
                ..
            }
        ));
    }

    #[test]
    fn a_second_call_with_the_same_id_gets_its_own_result() {
        let mut transcript = Transcript::new();
        let call = |seq| {
            agent(
                seq,
                AgentEvent::ToolCall {
                    id: "t".into(),
                    name: "Bash".into(),
                    input: json!({}),
                },
            )
        };
        let result = |seq, output: &str| {
            agent(
                seq,
                AgentEvent::ToolResult {
                    id: "t".into(),
                    output: output.into(),
                    is_error: false,
                },
            )
        };
        transcript.extend(&[call(1), result(2, "first"), call(3), result(4, "second")]);

        let outputs: Vec<Option<&str>> = transcript
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::Tool { output, .. } => Some(output.as_deref()),
                _ => None,
            })
            .collect();
        assert_eq!(outputs, vec![Some("first"), Some("second")]);
    }

    #[test]
    fn the_same_entry_arriving_twice_is_folded_once() {
        // Every event reaches a client both in the page it fetched and as a
        // push; folding both would double every reply.
        let mut transcript = Transcript::new();
        assert_eq!(transcript.apply(&text(1, "hello")), Applied::Added);
        assert_eq!(transcript.apply(&text(1, "hello")), Applied::AlreadySeen);
        assert_eq!(
            transcript.blocks(),
            &[Block::Assistant {
                text: "hello".into()
            }]
        );
    }

    #[test]
    fn an_entry_from_beyond_the_next_position_reports_the_hole() {
        let mut transcript = Transcript::new();
        transcript.apply(&text(1, "one"));
        assert_eq!(
            transcript.apply(&text(3, "three")),
            Applied::Gap { expected: 2 },
            "a transcript with a hole in it is worse than one that is refetched"
        );
        assert_eq!(transcript.cursor(), 1, "the gap is not swallowed");
        assert_eq!(transcript.blocks().len(), 1);
    }

    #[test]
    fn a_page_stops_at_the_first_hole_rather_than_folding_past_it() {
        let mut transcript = Transcript::new();
        let applied = transcript.extend(&[text(1, "one"), text(3, "three"), text(4, "four")]);
        assert_eq!(applied, Applied::Gap { expected: 2 });
        assert_eq!(transcript.cursor(), 1);
    }

    #[test]
    fn accounting_stays_out_of_the_conversation() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            text(1, "done"),
            agent(
                2,
                AgentEvent::Usage {
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 3,
                        ..Usage::default()
                    },
                },
            ),
        ]);
        assert_eq!(transcript.blocks().len(), 1);
        assert_eq!(transcript.usage().input_tokens, 10);
    }

    #[test]
    fn turn_boundaries_and_outcomes_are_drawn() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(1, AgentEvent::TurnEnd { turn: 1 }),
            agent(
                2,
                AgentEvent::SessionResult {
                    state: SessionState::Finished,
                    summary: Some("done".into()),
                },
            ),
        ]);
        assert_eq!(
            transcript.blocks(),
            &[
                Block::TurnEnd { turn: 1 },
                Block::Outcome {
                    state: SessionState::Finished,
                    summary: Some("done".into()),
                },
            ]
        );
    }

    #[test]
    fn long_tool_output_is_cut_with_a_note_rather_than_silently() {
        let output = (1..=30)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let shown = head_of(&output, 4);
        assert!(shown.starts_with("1\n2\n3\n4"));
        assert!(
            shown.ends_with("… 26 more lines"),
            "a reader has to know something was left out: {shown}"
        );
        assert_eq!(
            head_of("one\ntwo", 4),
            "one\ntwo",
            "short output is untouched"
        );
    }

    #[test]
    fn tool_arguments_read_as_what_the_agent_did() {
        assert_eq!(
            render_input(&json!({ "file_path": "src/main.rs" })),
            "file_path=src/main.rs"
        );
        assert_eq!(render_input(&json!({})), "");
        assert_eq!(render_input(&serde_json::Value::Null), "");
        assert_eq!(render_input(&json!("ls -la")), "ls -la");
    }
}
