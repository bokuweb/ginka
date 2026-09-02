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
use std::time::{Duration, Instant};

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

    /// The assistant text still being written, if that is what the last block
    /// is. This is the only part of a transcript that is revealed gradually.
    pub fn tail(&self) -> Option<&str> {
        match self.blocks.last() {
            Some(Block::Assistant { text }) => Some(text),
            _ => None,
        }
    }

    /// What the agent appears to be doing, for the line under the transcript.
    ///
    /// Read from the end of the transcript rather than from the session's
    /// state, because the session is `running` for the whole turn and the
    /// interesting question is what it is running *on*.
    pub fn activity(&self) -> Activity {
        match self.blocks.last() {
            Some(Block::Assistant { .. }) => Activity::Writing,
            Some(Block::Tool {
                name, output: None, ..
            }) => Activity::Running { tool: name.clone() },
            _ => Activity::Thinking,
        }
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

/// What the agent is doing, for the line under the transcript.
///
/// An agent between tokens looks identical to an agent that has died. Saying
/// which is which is the difference between waiting and wondering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Nothing has arrived for this turn yet.
    Thinking,
    /// A tool is running and has not answered.
    Running { tool: String },
    /// Text is arriving; the words themselves are the indicator.
    Writing,
}

// Every constant below is per second rather than per frame, because the frame
// is not a fixed length: the writing is driven by the display, and the same
// answer has to be written at the same speed on a 60Hz screen and a 120Hz one.
//
// The mechanism — and these numbers — are ported from bokuweb/pedro's chat
// reveal, which solved the same problem: an agent does not produce text evenly,
// and drawing exactly what has arrived puts its burstiness on the screen.

/// The slowest the text is ever written, in characters per second: the pace of
/// the trickle between bursts.
const SLOWEST: f32 = 90.;

/// The fastest. Well above what any CLI produces, so the writing can always
/// catch up in the end; it is the easing, not this ceiling, that keeps a burst
/// from landing as a block.
const FASTEST: f32 = 1200.;

/// How long the writing aims to take to drain what is waiting, in seconds.
/// Also how far behind the agent the text settles while a stream runs steadily.
const CATCH_UP: f32 = 0.6;

/// How quickly the rate moves towards that aim, as a time constant in seconds.
///
/// Easing the *rate* rather than the step is what stops a burst landing as a
/// block: stepping by a fraction of what is waiting puts the biggest jump on
/// the frame the chunk arrived, which is the chunk, redrawn.
const EASE: f32 = 0.25;

/// A frame is never treated as longer than this. A window that was occluded
/// should not dump a second of text in one step.
const LONGEST_FRAME: Duration = Duration::from_millis(100);

/// What the first frame is assumed to have taken.
const A_FRAME: Duration = Duration::from_millis(8);

/// Walks arrived text onto the screen at a pace a person can read.
///
/// The transcript already holds everything the agent has said; this decides
/// how much of the tail is on screen. History is shown whole — nobody wants to
/// watch a transcript they have already read being typed — and only text that
/// arrives while the window is watching is written out.
#[derive(Debug)]
pub struct Reveal {
    revealed: usize,
    rate: f32,
    carry: f32,
    last_frame: Option<Instant>,
}

impl Default for Reveal {
    fn default() -> Self {
        Self::new()
    }
}

impl Reveal {
    /// Nothing revealed, at the resting pace.
    pub fn new() -> Self {
        Self {
            revealed: 0,
            rate: SLOWEST,
            carry: 0.,
            last_frame: None,
        }
    }

    /// How many characters are on screen.
    pub fn revealed(&self) -> usize {
        self.revealed
    }

    /// Start again from nothing: a different session is a different answer.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Show everything at once.
    ///
    /// For a transcript that was read from storage rather than watched as it
    /// arrived: typing out a conversation the user has already had would be a
    /// pointless wait.
    pub fn show_all(&mut self, arrived: usize) {
        self.revealed = arrived;
        self.rate = SLOWEST;
        self.carry = 0.;
        self.last_frame = None;
    }

    /// Show a little more, and say whether anything is still hidden.
    ///
    /// Driven from the window's draw rather than a timer: at a fixed beat each
    /// step carries whatever arrived in that beat, which is a chunk however
    /// smoothly the rate was eased into it. The display is the clock that
    /// divides the same text into the most steps a reader can see.
    pub fn advance(&mut self, arrived: usize) -> bool {
        let now = Instant::now();
        let since = self
            .last_frame
            .replace(now)
            .map_or(A_FRAME, |last| now.saturating_duration_since(last))
            .min(LONGEST_FRAME);
        self.advance_over(arrived, since)
    }

    /// The same, over a frame of a stated length, which is what a test can
    /// hold still.
    fn advance_over(&mut self, arrived: usize, frame: Duration) -> bool {
        let waiting = arrived.saturating_sub(self.revealed);
        if waiting == 0 {
            // Come back at the resting pace rather than at whatever speed the
            // last burst worked it up to.
            self.rate = SLOWEST;
            self.carry = 0.;
            self.revealed = self.revealed.min(arrived);
            return false;
        }

        let seconds = frame.as_secs_f32();
        let aim = (waiting as f32 / CATCH_UP).clamp(SLOWEST, FASTEST);
        self.rate += (aim - self.rate) * (seconds / EASE).min(1.);

        self.carry += self.rate * seconds;
        let whole = self.carry.floor();
        self.carry -= whole;

        self.revealed = (self.revealed + whole as usize).min(arrived);
        self.revealed < arrived
    }

    /// The part of `text` that is on screen, cut on a character boundary.
    pub fn shown<'a>(&self, text: &'a str) -> &'a str {
        match text.char_indices().nth(self.revealed) {
            Some((at, _)) => &text[..at],
            None => text,
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
    format!(
        "{}\n{}",
        head.join("\n"),
        rust_i18n::t!("transcript.truncated", count = rest)
    )
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

    /// A frame at 120Hz, which is what the reveal is driven by.
    const FRAME: Duration = Duration::from_millis(8);

    #[test]
    fn a_burst_is_written_out_rather_than_landing_whole() {
        // An agent produces a hundred characters at once and then nothing for
        // a second; drawing exactly what arrived puts that on the screen.
        let mut reveal = Reveal::new();
        let arrived = 400;
        assert!(reveal.advance_over(arrived, FRAME));
        let after_one_frame = reveal.revealed();
        assert!(
            after_one_frame < arrived,
            "the whole burst landed in one frame: {after_one_frame}"
        );

        let mut frames = 1;
        while reveal.advance_over(arrived, FRAME) {
            frames += 1;
            assert!(frames < 10_000, "the writing never finished");
        }
        assert_eq!(reveal.revealed(), arrived);
        assert!(
            frames > 8,
            "it was written in {frames} frames, which is a jump"
        );
    }

    #[test]
    fn the_writing_never_runs_past_what_has_arrived() {
        let mut reveal = Reveal::new();
        while reveal.advance_over(20, FRAME) {}
        assert_eq!(reveal.revealed(), 20);
        assert!(
            !reveal.advance_over(20, FRAME),
            "it kept going after the end"
        );
    }

    #[test]
    fn it_comes_back_to_the_resting_pace_between_bursts() {
        // Otherwise the next burst starts at the speed the last one ended at,
        // and a one-word reply appears instantly.
        let mut reveal = Reveal::new();
        while reveal.advance_over(2_000, FRAME) {}
        let before = reveal.revealed();

        reveal.advance_over(before, FRAME);
        reveal.advance_over(before + 4, FRAME);
        assert!(
            reveal.revealed() - before <= 2,
            "the next burst started at the last one's speed"
        );
    }

    #[test]
    fn history_is_shown_whole_rather_than_typed_out() {
        // Reopening a workspace must not replay a conversation the user has
        // already read.
        let mut reveal = Reveal::new();
        reveal.show_all(500);
        assert_eq!(reveal.revealed(), 500);
        assert!(!reveal.advance_over(500, FRAME));
    }

    #[test]
    fn what_is_shown_is_cut_on_a_character_not_a_byte() {
        let mut reveal = Reveal::new();
        reveal.show_all(3);
        assert_eq!(reveal.shown("日本語です"), "日本語");
        reveal.show_all(99);
        assert_eq!(reveal.shown("日本語です"), "日本語です");
    }

    #[test]
    fn the_tail_is_the_only_thing_still_being_written() {
        let mut transcript = Transcript::new();
        transcript.extend(&[user(1, "hello"), text(2, "on it")]);
        assert_eq!(transcript.tail(), Some("on it"));

        // A finished turn has nothing left to write.
        transcript.apply(&agent(3, AgentEvent::TurnEnd { turn: 1 }));
        assert_eq!(transcript.tail(), None);
    }

    #[test]
    fn the_activity_says_what_the_agent_is_doing() {
        // An agent between tokens looks exactly like one that has died.
        let mut transcript = Transcript::new();
        transcript.apply(&user(1, "go"));
        assert_eq!(transcript.activity(), Activity::Thinking);

        transcript.apply(&agent(
            2,
            AgentEvent::ToolCall {
                id: "t".into(),
                name: "Bash".into(),
                input: json!({}),
            },
        ));
        assert_eq!(
            transcript.activity(),
            Activity::Running {
                tool: "Bash".into()
            }
        );

        transcript.apply(&agent(
            3,
            AgentEvent::ToolResult {
                id: "t".into(),
                output: "done".into(),
                is_error: false,
            },
        ));
        assert_eq!(
            transcript.activity(),
            Activity::Thinking,
            "a finished tool is not still running"
        );

        transcript.apply(&text(4, "here is what I found"));
        assert_eq!(transcript.activity(), Activity::Writing);
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
            shown.ends_with("26 more lines"),
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
