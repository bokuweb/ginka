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
use ginka_protocol::{AgentEvent, ContextUsage, SubagentStep, TaskItem, TaskStatus, Usage};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::terminal::{TerminalFileLink, file_links};

/// Resolve a completed turn inside its owning conversation. Workspace-wide
/// checkpoint lists can contain the same turn number from several sessions.
pub fn checkpoint_for_turn<'a>(
    checkpoints: &'a [ginka_protocol::Checkpoint],
    session: &ginka_protocol::SessionId,
    turn: u32,
) -> Option<&'a ginka_protocol::Checkpoint> {
    checkpoints
        .iter()
        .find(|point| &point.session == session && point.turn == turn && point.has_turn_start)
}

/// Markdown prepared for display with safe workspace file targets beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedMarkdown {
    /// Markdown source containing internal `ginka-file:` links.
    pub markdown: String,
    /// Targets addressed by the numeric suffix of each internal link.
    pub targets: Vec<TerminalFileLink>,
}

impl LinkedMarkdown {
    /// Resolve one internal href without treating arbitrary URLs as files.
    pub fn target(&self, href: &str) -> Option<&TerminalFileLink> {
        let index = href.strip_prefix("ginka-file:")?.parse::<usize>().ok()?;
        self.targets.get(index)
    }
}

/// Link safe file locations in prose while leaving fenced code untouched.
///
/// The detector is shared with the terminal so both surfaces enforce the same
/// daemon-host worktree boundary. Inline-code locations become linked code;
/// fenced output remains copyable verbatim.
pub fn link_file_locations(markdown: &str, worktree: &Path) -> LinkedMarkdown {
    let mut output = String::with_capacity(markdown.len());
    let mut targets = Vec::new();
    let mut fence: Option<char> = None;

    for line in markdown.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |body| (body, "\n"));
        let trimmed = body.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        if fence.is_some() || marker.is_some() {
            output.push_str(body);
            output.push_str(newline);
            if let Some(marker) = marker {
                fence = match fence {
                    Some(open) if open == marker => None,
                    None => Some(marker),
                    open => open,
                };
            }
            continue;
        }

        let characters = body.chars().collect::<Vec<_>>();
        let protected = markdown_link_ranges(&characters);
        let mut copied = 0;
        for target in file_links(body, worktree) {
            if protected
                .iter()
                .any(|(start, end)| target.columns.start < *end && target.columns.end > *start)
            {
                continue;
            }
            let mut start = target.columns.start;
            let mut end = target.columns.end;
            let inline_code = start > copied
                && end < characters.len()
                && characters[start - 1] == '`'
                && characters[end] == '`';
            if inline_code {
                start -= 1;
                end += 1;
            }
            if start < copied {
                continue;
            }
            output.extend(characters[copied..start].iter());
            let label = characters[start..end].iter().collect::<String>();
            let index = targets.len();
            output.push('[');
            output.push_str(&label.replace(']', "\\]"));
            output.push_str("](ginka-file:");
            output.push_str(&index.to_string());
            output.push(')');
            copied = end;
            targets.push(target);
        }
        output.extend(characters[copied..].iter());
        output.push_str(newline);
    }

    LinkedMarkdown {
        markdown: output,
        targets,
    }
}

fn markdown_link_ranges(characters: &[char]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < characters.len() {
        if characters[cursor] != '[' {
            cursor += 1;
            continue;
        }
        let Some(label_end) = characters[cursor + 1..]
            .iter()
            .position(|character| *character == ']')
            .map(|offset| cursor + 1 + offset)
        else {
            break;
        };
        if characters.get(label_end + 1) != Some(&'(') {
            cursor = label_end + 1;
            continue;
        }
        let Some(destination_end) = characters[label_end + 2..]
            .iter()
            .position(|character| *character == ')')
            .map(|offset| label_end + 2 + offset)
        else {
            break;
        };
        ranges.push((cursor, destination_end + 1));
        cursor = destination_end + 1;
    }
    ranges
}

/// One drawable piece of a transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Something the user sent.
    User {
        /// What the user typed, as sent.
        text: String,
    },
    /// Assistant prose, with its deltas already folded together.
    Assistant {
        /// The answer so far, as Markdown.
        text: String,
    },
    /// The agent's reasoning, where the vendor exposes it.
    Reasoning {
        /// The reasoning so far, as the vendor exposed it.
        text: String,
    },
    /// A tool call and, once it arrives, its result.
    Tool {
        /// Correlates the call with its result.
        id: String,
        /// The normalized kind of tool, shown as the block's label.
        name: String,
        /// The call's one-line title, the line the reader scans.
        input: String,
        /// `None` while the tool is still running.
        output: Option<String>,
        /// Whether the tool reported a failure.
        is_error: bool,
    },
    /// The newest complete snapshot from an agent-maintained task list.
    Tasks {
        /// Provider-neutral rows in the order chosen by the agent.
        items: Vec<TaskItem>,
    },
    /// A delegated agent and the bounded trail of work it reported.
    Subagent {
        /// Provider call id correlating steps and the final report.
        id: String,
        /// The brief the parent gave this run.
        title: String,
        /// Steps merged by provider id, newest steps retained at the cap.
        steps: Vec<SubagentStep>,
        /// The final report returned to the parent, when available.
        summary: Option<String>,
        /// Whether the delegated run failed.
        is_error: bool,
    },
    /// The agent is blocked on the user.
    Question {
        /// What an answer is sent against.
        id: String,
        /// What the agent asked, or the permission it wants.
        question: String,
        /// Choices the agent offered; empty for a free-form answer.
        options: Vec<String>,
        /// The questions as the agent structured them, drawn as a form with
        /// descriptions, several picks and an *Other* field
        /// ([`crate::question_form`]); empty for an approval or a plain
        /// question.
        questions: Vec<ginka_protocol::question::Question>,
        /// Whether the reader has already replied to it.
        ///
        /// Kept on the block rather than in the view: a transcript re-read
        /// after a restart has to know as much as one that was watched, and
        /// what it knows is that the reader said something afterwards.
        answered: bool,
    },
    /// A plan the agent wants approved before acting.
    Plan {
        /// What an approval or rejection is sent against.
        id: String,
        /// The proposed plan, as Markdown.
        plan: String,
        /// Whether the reader has already approved or rejected it.
        answered: bool,
    },
    /// A turn boundary. A checkpoint was taken here.
    TurnEnd {
        /// One-based number of the turn that ended.
        turn: u32,
        /// The inclusive transcript position copied by a fork from here.
        seq: u64,
        /// Provider that ran this turn, retained independently of later turns.
        provider: Option<String>,
        /// Requested or provider-reported model for this turn.
        model: Option<String>,
        /// Requested reasoning level for this turn.
        reasoning_effort: Option<String>,
        /// Requested service tier for this turn.
        service_tier: Option<String>,
    },
    /// How the session ended.
    Outcome {
        /// The terminal state the session reached.
        state: SessionState,
        /// The driver's closing summary, when it gave one.
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
    Gap {
        /// The position that should have come next; re-read from here.
        expected: u64,
    },
}

/// A session's transcript, folded.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    blocks: Vec<Block>,
    /// The first block changed since [`Transcript::take_changed`] was last
    /// asked, so a virtualized list re-measures from there and no earlier.
    changed_from: Option<usize>,
    /// Drawable block indexes for top-level user prompts. Interaction-card
    /// answers are user blocks too, but are not new turns in the outline.
    prompt_indices: Vec<usize>,
    /// Drawable block for each stored sequence position. Hidden bookkeeping
    /// entries are `None`; positions are one-based while this vector is zero-based.
    positions: Vec<Option<usize>>,
    /// Positions before the first one folded here: nonzero for a transcript
    /// that starts partway through its session.
    skipped: u64,
    cursor: u64,
    usage: Usage,
    context_usage: Option<ContextUsage>,
    turn_provider: Option<String>,
    turn_model: Option<String>,
    turn_reasoning_effort: Option<String>,
    turn_service_tier: Option<String>,
    /// When the prompt being answered was sent, so the turn's end can say
    /// how long it took.
    asked_at: Option<i64>,
    /// When the entry being folded was recorded.
    folding_at: i64,
    /// How long each turn took and when it ended, by the index of its
    /// `TurnEnd` block. Kept beside the blocks rather than in them: it is a
    /// reading of the entries' clocks, not something the agent said.
    turn_times: std::collections::HashMap<usize, TurnTiming>,
}

/// How long a turn took, and when it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnTiming {
    /// From the prompt to the turn's end. `None` when the prompt is not in
    /// this transcript — a session that began before it was recorded.
    pub seconds: Option<u64>,
    /// When the turn ended, in Unix seconds.
    pub ended_at: i64,
}

/// How long a turn took, as the line under it says it: `3s`, `2m 5s`,
/// `1h 4m`. Seconds are dropped past an hour — nobody waiting an hour is
/// counting them.
pub fn duration_label(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => match seconds % 60 {
            0 => format!("{}m", seconds / 60),
            rest => format!("{}m {rest}s", seconds / 60),
        },
        _ => format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

impl Transcript {
    /// How long the turn that ends at block `index` took, and when it ended.
    pub fn turn_timing(&self, index: usize) -> Option<TurnTiming> {
        self.turn_times.get(&index).copied()
    }

    /// An empty transcript, positioned before the first entry.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty transcript that folds from position `first` onward: a long
    /// session's latest page, with the earlier ones read on request.
    pub fn starting_at(first: u64) -> Self {
        let skipped = first.saturating_sub(1);
        Self {
            cursor: skipped,
            skipped,
            ..Self::default()
        }
    }

    /// The first position this transcript folds from.
    pub fn first_seq(&self) -> u64 {
        self.skipped + 1
    }

    /// Whether the session has entries before the ones folded here.
    pub fn has_earlier(&self) -> bool {
        self.skipped > 0
    }

    /// The highest position folded in. This is the cursor to page from.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// What the window draws.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Resolve one persisted transcript position to the block that folded it.
    pub fn block_index_for_seq(&self, seq: u64) -> Option<usize> {
        usize::try_from(seq)
            .ok()
            .and_then(|seq| seq.checked_sub(1 + self.skipped as usize))
            .and_then(|index| self.positions.get(index))
            .copied()
            .flatten()
    }

    /// The persisted position a drawable block was folded from — the first,
    /// where several entries folded into one. What an edit of a prompt names
    /// the prompt by.
    pub fn seq_for_block(&self, index: usize) -> Option<u64> {
        self.positions
            .iter()
            .position(|block| *block == Some(index))
            .map(|position| position as u64 + 1 + self.skipped)
    }

    /// Top-level prompts in reading order, paired with their drawable block.
    ///
    /// Answers to an agent question or plan card are deliberately absent: the
    /// outline is turn navigation, not a second rendering of every user block.
    pub fn prompt_outline(&self) -> impl ExactSizeIterator<Item = (usize, &str)> {
        self.prompt_indices.iter().map(|index| {
            let Block::User { text } = &self.blocks[*index] else {
                unreachable!("prompt indexes are recorded only for user blocks")
            };
            (*index, text.as_str())
        })
    }

    /// The newest unanswered card that asks structured questions, with its id:
    /// what the window pins above the composer as a form.
    pub fn open_questions(&self) -> Option<(&str, &[ginka_protocol::question::Question])> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Question {
                id,
                questions,
                answered: false,
                ..
            } if !questions.is_empty() => Some((id.as_str(), questions.as_slice())),
            _ => None,
        })
    }

    /// The newest question or plan still waiting for an answer.
    ///
    /// Free-text answers typed in the composer are addressed here; option
    /// buttons carry their request id directly.
    pub fn open_request(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Question {
                id,
                answered: false,
                ..
            }
            | Block::Plan {
                id,
                answered: false,
                ..
            } => Some(id.as_str()),
            _ => None,
        })
    }

    /// Whether no block has been folded in yet; with nothing running, the centre column then shows the home screen.
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
            // The title, not the kind: "cargo test" says what the agent is
            // doing where "run" only says what sort of thing it is.
            Some(Block::Tool {
                input,
                output: None,
                ..
            }) => Activity::Running {
                tool: input.clone(),
            },
            Some(Block::Subagent {
                title,
                summary: None,
                ..
            }) => Activity::Running {
                tool: title.clone(),
            },
            Some(Block::Tasks { items }) => items
                .iter()
                .find(|item| item.status == TaskStatus::InProgress)
                .map_or(Activity::Thinking, |item| Activity::Running {
                    tool: item.label.clone(),
                }),
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

    /// The latest provider-reported context occupancy, kept apart from
    /// cumulative session accounting because compaction can reduce it.
    pub fn context_usage(&self) -> Option<ContextUsage> {
        self.context_usage
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
        self.folding_at = entry.at;

        let block = match &entry.payload {
            TranscriptPayload::User { text } => {
                self.asked_at = Some(entry.at);
                self.blocks.push(Block::User { text: text.clone() });
                let index = self.blocks.len() - 1;
                self.prompt_indices.push(index);
                Some(index)
            }
            TranscriptPayload::Response { request_id, text } => {
                self.answer(request_id);
                self.blocks.push(Block::User { text: text.clone() });
                Some(self.blocks.len() - 1)
            }
            TranscriptPayload::Agent { event } => {
                if matches!(event, AgentEvent::TurnEnd { .. })
                    && let Some(&prompt) = self.prompt_indices.last()
                {
                    // Settling a turn can close cards anywhere in it.
                    self.touch(prompt);
                }
                self.fold(event, entry.seq)
            }
        };
        if let Some(index) = block {
            self.touch(index);
        }
        self.positions.push(block);
        Applied::Added
    }

    /// The first block that changed since this was last asked, if any, and
    /// forget it: a view re-measures from there.
    pub fn take_changed(&mut self) -> Option<usize> {
        self.changed_from.take()
    }

    fn touch(&mut self, index: usize) {
        self.changed_from = Some(self.changed_from.map_or(index, |from| from.min(index)));
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

    fn fold(&mut self, event: &AgentEvent, seq: u64) -> Option<usize> {
        match event {
            AgentEvent::TextDelta { text } => self.append_text(text),
            AgentEvent::Reasoning { text } => self.append_reasoning(text),
            // The driver already normalized the call into one shape; the block
            // keeps the kind as its label and the title as the line the reader
            // scans.
            AgentEvent::ToolCall { activity } => {
                if let Some(tasks) = &activity.tasks {
                    return self.upsert_tasks(tasks.clone());
                }
                self.blocks.push(Block::Tool {
                    id: activity.id.clone().unwrap_or_default(),
                    name: activity.kind_str().to_string(),
                    input: activity.title.clone(),
                    output: None,
                    is_error: false,
                });
                Some(self.blocks.len() - 1)
            }
            AgentEvent::ToolResult { activity } => {
                if let Some(tasks) = &activity.tasks {
                    self.upsert_tasks(tasks.clone())
                } else {
                    self.attach_result(
                        activity.id.as_deref().unwrap_or_default(),
                        activity.detail.as_deref().unwrap_or_default(),
                        activity.failed,
                    )
                }
            }
            AgentEvent::SubagentStarted { id, title } => {
                self.blocks.push(Block::Subagent {
                    id: id.clone(),
                    title: title.clone(),
                    steps: Vec::new(),
                    summary: None,
                    is_error: false,
                });
                Some(self.blocks.len() - 1)
            }
            AgentEvent::SubagentStep { parent_id, step } => {
                self.attach_subagent_step(parent_id, step)
            }
            AgentEvent::SubagentFinished {
                id,
                summary,
                failed,
            } => self.finish_subagent(id, summary.clone(), *failed),
            AgentEvent::AskUser {
                id,
                question,
                options,
                questions,
            } => {
                self.blocks.push(Block::Question {
                    id: id.clone(),
                    question: question.clone(),
                    options: options.clone(),
                    questions: questions.clone(),
                    answered: false,
                });
                Some(self.blocks.len() - 1)
            }
            AgentEvent::PlanProposal { id, plan } => {
                self.blocks.push(Block::Plan {
                    id: id.clone(),
                    plan: plan.clone(),
                    answered: false,
                });
                Some(self.blocks.len() - 1)
            }
            AgentEvent::Permission { id, request } => {
                self.blocks.push(Block::Question {
                    id: id.clone(),
                    question: request.clone(),
                    options: Vec::new(),
                    questions: Vec::new(),
                    answered: false,
                });
                Some(self.blocks.len() - 1)
            }
            // Accounting belongs in the context bar, not in the conversation,
            // and the account's windows belong to the account chip.
            AgentEvent::Usage { usage } => {
                self.usage = *usage;
                None
            }
            AgentEvent::ContextUsage { usage } => {
                self.context_usage = Some(*usage);
                None
            }
            AgentEvent::PlanUsage { .. } => None,
            AgentEvent::TurnEnd { turn } => {
                self.blocks.push(Block::TurnEnd {
                    turn: *turn,
                    seq,
                    provider: self.turn_provider.take(),
                    model: self.turn_model.take(),
                    reasoning_effort: self.turn_reasoning_effort.take(),
                    service_tier: self.turn_service_tier.take(),
                });
                let index = self.blocks.len() - 1;
                let seconds = self
                    .asked_at
                    .take()
                    .map(|asked| (self.folding_at - asked).max(0) as u64);
                self.turn_times.insert(
                    index,
                    TurnTiming {
                        seconds,
                        ended_at: self.folding_at,
                    },
                );
                Some(index)
            }
            AgentEvent::SessionResult { state, summary } => {
                self.blocks.push(Block::Outcome {
                    state: *state,
                    summary: summary.clone(),
                });
                Some(self.blocks.len() - 1)
            }
            // A shape this build does not understand is shown, not dropped: it
            // is how a vendor's format change first reaches a reader (R6).
            AgentEvent::Unsupported { shape } => {
                self.append_text(&format!("(not understood: {shape})"))
            }
            AgentEvent::TurnStarted {
                provider,
                model,
                reasoning_effort,
                service_tier,
            } => {
                self.turn_provider.clone_from(provider);
                self.turn_model.clone_from(model);
                self.turn_reasoning_effort.clone_from(reasoning_effort);
                self.turn_service_tier.clone_from(service_tier);
                None
            }
            // The provider's own answer wins over a requested alias. Keep the
            // supervisor's model when a connection event names none.
            AgentEvent::Connected { model, .. } => {
                if model.is_some() {
                    self.turn_model.clone_from(model);
                }
                None
            }
            // Session bookkeeping the conversation does not render.
            AgentEvent::Commands { .. }
            | AgentEvent::SteerAccepted
            | AgentEvent::SteerRejected { .. }
            | AgentEvent::AgentTitle { .. }
            | AgentEvent::ProcessExited { .. } => None,
        }
    }

    /// Mark one question or plan answered, before the daemon says so.
    ///
    /// A card that stays clickable after it has been clicked invites a second
    /// answer to a question that has one.
    pub fn answer(&mut self, id: &str) {
        let mut touched = None;
        for (index, block) in self.blocks.iter_mut().enumerate() {
            match block {
                Block::Question {
                    id: asked,
                    answered,
                    ..
                }
                | Block::Plan {
                    id: asked,
                    answered,
                    ..
                } if asked == id => {
                    *answered = true;
                    touched.get_or_insert(index);
                }
                _ => {}
            }
        }
        if let Some(index) = touched {
            self.touch(index);
        }
    }

    /// Grow the open assistant paragraph, or start one.
    fn append_text(&mut self, text: &str) -> Option<usize> {
        match self.blocks.last_mut() {
            Some(Block::Assistant { text: existing }) => {
                existing.push_str(text);
                Some(self.blocks.len() - 1)
            }
            _ => {
                self.blocks.push(Block::Assistant {
                    text: text.to_string(),
                });
                Some(self.blocks.len() - 1)
            }
        }
    }

    fn append_reasoning(&mut self, text: &str) -> Option<usize> {
        match self.blocks.last_mut() {
            Some(Block::Reasoning { text: existing }) => {
                existing.push_str(text);
                Some(self.blocks.len() - 1)
            }
            _ => {
                self.blocks.push(Block::Reasoning {
                    text: text.to_string(),
                });
                Some(self.blocks.len() - 1)
            }
        }
    }

    /// Attach a result to the call that asked for it.
    ///
    /// Searched from the end because a result belongs to the most recent call
    /// with that id, and an id a driver had to synthesise may repeat across a
    /// long session. A result with no call is still shown: losing output
    /// because the call was in a page we have not fetched would be worse than
    /// an unattached card.
    fn attach_result(&mut self, id: &str, output: &str, is_error: bool) -> Option<usize> {
        let matching = self.blocks.iter().rposition(
            |block| matches!(block, Block::Tool { id: call, output: None, .. } if call == id),
        );
        match matching {
            Some(index) => {
                let Block::Tool {
                    output: slot,
                    is_error: failed,
                    ..
                } = &mut self.blocks[index]
                else {
                    unreachable!()
                };
                *slot = Some(output.to_string());
                *failed = is_error;
                Some(index)
            }
            None => {
                self.blocks.push(Block::Tool {
                    id: id.to_string(),
                    name: String::new(),
                    input: String::new(),
                    output: Some(output.to_string()),
                    is_error,
                });
                Some(self.blocks.len() - 1)
            }
        }
    }

    /// Replace the current turn's task snapshot, or start its one task card.
    fn upsert_tasks(&mut self, items: Vec<TaskItem>) -> Option<usize> {
        let boundary = self
            .blocks
            .iter()
            .rposition(|block| matches!(block, Block::TurnEnd { .. }))
            .map_or(0, |index| index + 1);
        if let Some(index) = self.blocks[boundary..]
            .iter()
            .rposition(|block| matches!(block, Block::Tasks { .. }))
            .map(|index| boundary + index)
        {
            self.blocks[index] = Block::Tasks { items };
            return Some(index);
        }
        self.blocks.push(Block::Tasks { items });
        Some(self.blocks.len() - 1)
    }

    /// Number of delegated-agent steps retained in one drawable row.
    const MAX_SUBAGENT_STEPS: usize = 300;

    /// Merge a delegated tool lifecycle update instead of adding a second row.
    fn attach_subagent_step(&mut self, parent_id: &str, step: &SubagentStep) -> Option<usize> {
        let index = self
            .blocks
            .iter()
            .rposition(|block| matches!(block, Block::Subagent { id, .. } if id == parent_id))?;
        let Block::Subagent { steps, .. } = &mut self.blocks[index] else {
            unreachable!()
        };
        if let Some(existing) = steps.iter_mut().find(|existing| existing.id == step.id) {
            let text = (!step.text.is_empty()).then(|| step.text.clone());
            existing.kind = step.kind;
            existing.status = step.status;
            if let Some(text) = text {
                existing.text = text;
            }
            return Some(index);
        }
        steps.push(step.clone());
        if steps.len() > Self::MAX_SUBAGENT_STEPS {
            steps.remove(0);
        }
        Some(index)
    }

    /// Settle the delegated row without allowing its report to rename it.
    fn finish_subagent(
        &mut self,
        id: &str,
        summary: Option<String>,
        failed: bool,
    ) -> Option<usize> {
        let index = self.blocks.iter().rposition(
            |block| matches!(block, Block::Subagent { id: parent, .. } if parent == id),
        )?;
        let Block::Subagent {
            summary: output,
            is_error,
            ..
        } = &mut self.blocks[index]
        else {
            unreachable!()
        };
        // `Some("")` is a completed run whose provider had no report. Keeping
        // completion separate from visible text prevents the activity line
        // from claiming the child is still working forever.
        *output = Some(summary.unwrap_or_default());
        *is_error = failed;
        Some(index)
    }
}

/// Turn a prompt into the single bounded line shown by the outline.
///
/// The limit counts Unicode scalar values rather than bytes, so truncating a
/// Japanese prompt cannot cut through its UTF-8 representation.
pub fn prompt_outline_label(prompt: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let one_line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max_chars {
        return one_line;
    }
    let mut label = one_line
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    label.push('…');
    label
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
    Running {
        /// What is running, in the reader's terms: the call's title, a delegated
        /// agent's brief or the task in progress.
        tool: String,
    },
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

/// Split assistant text into the part that is safe to format and the part
/// that is not.
///
/// Markdown of half a document is not markdown of anything: a heading with no
/// line after it, a fence with no closing fence. So the text is cut at the
/// last blank line — the end of the last block that is definitely finished —
/// and only what precedes it is formatted. The tail is drawn as the plain text
/// it still is, and moves across as soon as its block is done.
///
/// The cut never lands inside a fenced block, which a blank line does not end.
/// Ported from bokuweb/pedro, which hit the same problem: a markdown view
/// re-parsing text that changes every frame makes a streaming answer land in
/// slabs.
pub fn settled(text: &str) -> (&str, &str) {
    let mut cut = 0;
    let mut fences = 0;
    let mut at = 0;

    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fences += 1;
        }
        at += line.len();
        if line.trim().is_empty() && fences % 2 == 0 {
            cut = at;
        }
    }

    text.split_at(cut)
}

/// Choose the exact selected range for quoting, or the owning message.
///
/// Window text selection is allowed to contain leading and trailing space;
/// only an all-whitespace selection is treated as absent.
pub fn quote_target<'a>(message: &'a str, selection: &'a str) -> &'a str {
    if selection.trim().is_empty() {
        message
    } else {
        selection
    }
}

/// Return an exact selection only when quoting it would add meaningful text.
pub fn selected_quote(selection: &str) -> Option<&str> {
    (!selection.trim().is_empty()).then_some(selection)
}

/// Completed and total actionable tasks for a compact progress label.
///
/// Cancelled work remains visible in the card but is not work left to finish.
pub fn task_progress(items: &[TaskItem]) -> (usize, usize) {
    let completed = items
        .iter()
        .filter(|item| item.status == TaskStatus::Completed)
        .count();
    let total = items
        .iter()
        .filter(|item| item.status != TaskStatus::Cancelled)
        .count();
    (completed, total)
}

/// One compact provenance label for a completed turn.
///
/// Empty legacy fields disappear rather than leaving punctuation behind.
pub fn turn_provenance(
    provider: Option<&str>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
) -> Option<String> {
    let parts = [provider, model, reasoning_effort, service_tier]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Whether to pull the transcript to its foot, and whether it is still
/// following.
///
/// Told by the reader's gesture rather than worked out from where the view
/// ends up: an answer that grows moves the foot away from the reader too, and a
/// rule that could not tell those apart would either stop following on its own
/// or drag the reader back from a paragraph they had gone to read. Ported from
/// bokuweb/pedro.
pub fn following(working: bool, follows: bool, at_foot: bool) -> (bool, bool) {
    let follows = follows || at_foot;
    (working && follows, follows)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_can_start_partway_and_still_find_its_positions() {
        // A long session opens on its latest page; the earlier ones are
        // read when the reader asks for them.
        let mut transcript = Transcript::starting_at(5);
        assert!(transcript.has_earlier());
        assert_eq!(transcript.first_seq(), 5);
        assert!(matches!(
            transcript.apply(&user(4, "before")),
            Applied::AlreadySeen
        ));
        assert!(matches!(
            transcript.apply(&user(6, "skipped")),
            Applied::Gap { expected: 5 }
        ));
        transcript.extend(&[user(5, "five"), text(6, "answer"), user(7, "seven")]);
        assert_eq!(transcript.cursor(), 7);
        assert_eq!(transcript.block_index_for_seq(5), Some(0));
        assert_eq!(transcript.block_index_for_seq(7), Some(2));
        assert_eq!(transcript.block_index_for_seq(4), None, "not folded here");
        assert_eq!(transcript.seq_for_block(2), Some(7));
        assert_eq!(transcript.prompt_outline().count(), 2);

        let whole = Transcript::new();
        assert!(
            !whole.has_earlier(),
            "a transcript from the start has nothing before it"
        );
        assert_eq!(whole.first_seq(), 1);
    }

    #[test]
    fn what_changed_is_reported_from_the_first_block_it_touched_and_then_forgotten() {
        let mut transcript = Transcript::new();
        assert_eq!(transcript.take_changed(), None);

        transcript.extend(&[user(1, "one"), text(2, "hel")]);
        assert_eq!(transcript.take_changed(), Some(0));
        assert_eq!(transcript.take_changed(), None, "taken");

        // The answer growing touches only the answer.
        transcript.apply(&text(3, "lo"));
        assert_eq!(transcript.take_changed(), Some(1));

        // A result arriving after later blocks goes back to its call.
        let mut call = ActivityItem::from_tool(Some("c1".into()), "tool", &json!({}));
        transcript.apply(&agent(
            4,
            AgentEvent::ToolCall {
                activity: call.clone(),
            },
        ));
        let call_block = transcript.blocks().len() - 1;
        transcript.apply(&text(5, "meanwhile"));
        transcript.take_changed();
        call.complete_with("done", false);
        transcript.apply(&agent(6, AgentEvent::ToolResult { activity: call }));
        assert_eq!(transcript.take_changed(), Some(call_block));
    }

    #[test]
    fn answering_a_question_changes_its_card() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            user(1, "go"),
            agent(
                2,
                AgentEvent::AskUser {
                    id: "q1".into(),
                    question: "which?".into(),
                    options: vec!["a".into()],
                    questions: Vec::new(),
                },
            ),
            text(3, "waiting"),
        ]);
        transcript.take_changed();
        transcript.answer("q1");
        assert_eq!(transcript.take_changed(), Some(1));
    }

    #[test]
    fn a_turns_end_rereads_the_turn_it_ends() {
        // Settling a turn can close cards anywhere in it — delegated agents
        // that never reported — so the whole turn is measured again.
        let mut transcript = Transcript::new();
        transcript.extend(&[
            user(1, "first"),
            text(2, "a"),
            user(3, "second"),
            text(4, "b"),
        ]);
        transcript.take_changed();
        transcript.apply(&agent(5, AgentEvent::TurnEnd { turn: 2 }));
        assert_eq!(transcript.take_changed(), Some(2));
    }

    #[test]
    fn a_block_names_the_position_it_was_folded_from() {
        let mut transcript = Transcript::new();
        transcript.extend(&[user(1, "one"), user(2, "two")]);
        assert_eq!(transcript.seq_for_block(0), Some(1));
        assert_eq!(transcript.seq_for_block(1), Some(2));
        assert_eq!(transcript.seq_for_block(9), None);
    }
    use ginka_protocol::event::ActivityItem;
    use ginka_protocol::{SubagentStep, SubagentStepKind, SubagentStepStatus};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn quoting_prefers_the_exact_selected_range_and_falls_back_to_the_message() {
        assert_eq!(
            quote_target("the whole answer", "chosen words"),
            "chosen words"
        );
        assert_eq!(
            quote_target("the whole answer", "  chosen words  \n"),
            "  chosen words  \n"
        );
        assert_eq!(
            quote_target("the whole answer", " \n\t"),
            "the whole answer"
        );
    }

    #[test]
    fn the_selection_quote_action_exists_only_for_meaningful_text() {
        assert_eq!(
            selected_quote("  exact range  \n"),
            Some("  exact range  \n")
        );
        assert_eq!(selected_quote(" \n\t"), None);
        assert_eq!(selected_quote(""), None);
    }

    #[test]
    fn prose_file_locations_become_internal_markdown_links() {
        let linked = link_file_locations(
            "See src/main.rs:12:3 and `crates/ginka-ui/src/lib.rs:8`.",
            Path::new("/work/ginka"),
        );

        assert_eq!(
            linked.markdown,
            "See [src/main.rs:12:3](ginka-file:0) and [`crates/ginka-ui/src/lib.rs:8`](ginka-file:1)."
        );
        assert_eq!(linked.targets.len(), 2);
        assert_eq!(linked.targets[0].path, "src/main.rs");
        assert_eq!(linked.targets[0].line, Some(12));
        assert_eq!(linked.targets[0].column, Some(3));
        assert_eq!(linked.target("ginka-file:1").unwrap().line, Some(8));
        assert!(linked.target("https://example.test").is_none());
    }

    #[test]
    fn fenced_code_and_existing_markdown_links_are_not_rewritten() {
        let source = "[docs](https://example.test/src/main.rs:1) [guide](docs/readme.md)\n\n```text\nsrc/main.rs:2\n```\n";
        let linked = link_file_locations(source, Path::new("/work/ginka"));

        assert_eq!(linked.markdown, source);
        assert!(linked.targets.is_empty());
    }

    #[test]
    fn transcript_file_links_share_the_terminal_workspace_boundary() {
        let linked = link_file_locations(
            "../secret.txt:1 /elsewhere/secret.txt:2 https://example.test/a.rs:3",
            Path::new("/work/ginka"),
        );

        assert!(linked.targets.is_empty());
        assert_eq!(
            linked.markdown,
            "../secret.txt:1 /elsewhere/secret.txt:2 https://example.test/a.rs:3"
        );
    }

    #[test]
    fn versions_and_domain_names_are_not_presented_as_files() {
        let source = "Version 1.2 fixes example.com while README.md has details.";
        let linked = link_file_locations(source, Path::new("/work/ginka"));

        assert_eq!(
            linked.markdown,
            "Version 1.2 fixes example.com while [README.md](ginka-file:0) has details."
        );
        assert_eq!(linked.targets.len(), 1);
        assert_eq!(linked.targets[0].path, "README.md");
    }

    fn user(seq: u64, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            seq,
            at: 0,
            payload: TranscriptPayload::User { text: text.into() },
        }
    }

    /// A tool call that already produced its result.
    fn completed(id: &str, output: &str, failed: bool) -> ActivityItem {
        let mut activity = ActivityItem::from_tool(Some(id.into()), "tool", &json!({}));
        activity.complete_with(output, failed);
        activity
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
    fn a_persisted_response_closes_only_its_interaction_card() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::AskUser {
                    id: "database".into(),
                    question: "Which database?".into(),
                    options: Vec::new(),
                    questions: Vec::new(),
                },
            ),
            agent(
                2,
                AgentEvent::PlanProposal {
                    id: "plan".into(),
                    plan: "Create the schema".into(),
                },
            ),
            TranscriptEntry {
                seq: 3,
                at: 0,
                payload: TranscriptPayload::Response {
                    request_id: "database".into(),
                    text: "SQLite".into(),
                },
            },
        ]);

        assert!(matches!(
            &transcript.blocks()[0],
            Block::Question { answered: true, .. }
        ));
        assert!(matches!(
            &transcript.blocks()[1],
            Block::Plan {
                answered: false,
                ..
            }
        ));
    }

    #[test]
    fn the_composer_answers_the_newest_open_interaction() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::AskUser {
                    id: "old".into(),
                    question: "First?".into(),
                    options: Vec::new(),
                    questions: Vec::new(),
                },
            ),
            agent(
                2,
                AgentEvent::AskUser {
                    id: "new".into(),
                    question: "Second?".into(),
                    options: Vec::new(),
                    questions: Vec::new(),
                },
            ),
        ]);
        assert_eq!(transcript.open_request(), Some("new"));

        transcript.answer("new");
        assert_eq!(transcript.open_request(), Some("old"));
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
                    activity: ActivityItem::from_tool(
                        Some("t1".into()),
                        "Read",
                        &json!({ "file_path": "a.rs" }),
                    ),
                },
            ),
            text(2, "reading"),
            agent(
                3,
                AgentEvent::ToolResult {
                    activity: completed("t1", "fn main() {}", false),
                },
            ),
        ]);
        assert_eq!(
            transcript.blocks()[0],
            Block::Tool {
                id: "t1".into(),
                name: "tool".into(),
                input: "a.rs".into(),
                output: Some("fn main() {}".into()),
                is_error: false,
            }
        );
    }

    #[test]
    fn task_updates_replace_the_live_card_and_hide_provider_tool_chrome() {
        let task_activity = |id: &str, tasks: serde_json::Value| {
            ActivityItem::from_tool(Some(id.into()), "TodoWrite", &json!({ "todos": tasks }))
        };
        let first = task_activity(
            "tasks-1",
            json!([
                {"content": "Inspect", "status": "completed"},
                {"content": "Implement", "status": "in_progress"}
            ]),
        );
        let second = task_activity(
            "tasks-2",
            json!([
                {"content": "Inspect", "status": "completed"},
                {"content": "Implement", "status": "completed"},
                {"content": "Verify", "status": "pending"}
            ]),
        );
        let mut result = second.clone();
        result.complete_with("tasks updated", false);
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(1, AgentEvent::ToolCall { activity: first }),
            agent(2, AgentEvent::ToolCall { activity: second }),
            agent(3, AgentEvent::ToolResult { activity: result }),
        ]);

        assert_eq!(
            transcript.blocks(),
            &[Block::Tasks {
                items: vec![
                    TaskItem::new("Inspect", TaskStatus::Completed),
                    TaskItem::new("Implement", TaskStatus::Completed),
                    TaskItem::new("Verify", TaskStatus::Pending),
                ],
            }]
        );
        assert_eq!(transcript.block_index_for_seq(1), Some(0));
        assert_eq!(transcript.block_index_for_seq(2), Some(0));
        assert_eq!(transcript.block_index_for_seq(3), Some(0));
    }

    #[test]
    fn task_progress_excludes_cancelled_work_from_the_completion_count() {
        let items = vec![
            TaskItem::new("Done", TaskStatus::Completed),
            TaskItem::new("Current", TaskStatus::InProgress),
            TaskItem::new("Later", TaskStatus::Pending),
            TaskItem::new("Skipped", TaskStatus::Cancelled),
        ];

        assert_eq!(task_progress(&items), (1, 3));
    }

    #[test]
    fn subagent_steps_merge_by_id_and_finish_under_one_parent_row() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::SubagentStarted {
                    id: "agent-1".into(),
                    title: "Review correctness".into(),
                },
            ),
            agent(
                2,
                AgentEvent::SubagentStep {
                    parent_id: "agent-1".into(),
                    step: SubagentStep::new("read-1", SubagentStepKind::Tool, "Read src/state.rs")
                        .with_status(SubagentStepStatus::Running),
                },
            ),
            agent(
                3,
                AgentEvent::SubagentStep {
                    parent_id: "agent-1".into(),
                    step: SubagentStep::new("read-1", SubagentStepKind::Tool, "")
                        .with_status(SubagentStepStatus::Completed),
                },
            ),
            agent(
                4,
                AgentEvent::SubagentFinished {
                    id: "agent-1".into(),
                    summary: Some("No regressions found.".into()),
                    failed: false,
                },
            ),
        ]);

        assert_eq!(
            transcript.blocks(),
            [Block::Subagent {
                id: "agent-1".into(),
                title: "Review correctness".into(),
                steps: vec![
                    SubagentStep::new("read-1", SubagentStepKind::Tool, "Read src/state.rs",)
                        .with_status(SubagentStepStatus::Completed)
                ],
                summary: Some("No regressions found.".into()),
                is_error: false,
            }]
        );
    }

    #[test]
    fn orphan_subagent_updates_are_dropped() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::SubagentStep {
                    parent_id: "missing-agent".into(),
                    step: SubagentStep::new(
                        "message-1:text",
                        SubagentStepKind::Message,
                        "private child output",
                    ),
                },
            ),
            agent(
                2,
                AgentEvent::SubagentFinished {
                    id: "missing-agent".into(),
                    summary: Some("private child report".into()),
                    failed: false,
                },
            ),
        ]);

        assert!(transcript.blocks().is_empty());
    }

    #[test]
    fn a_subagent_trail_keeps_only_its_newest_bounded_steps() {
        let mut entries = vec![agent(
            1,
            AgentEvent::SubagentStarted {
                id: "agent-1".into(),
                title: "Review correctness".into(),
            },
        )];
        entries.extend((0..=Transcript::MAX_SUBAGENT_STEPS).map(|index| {
            agent(
                index as u64 + 2,
                AgentEvent::SubagentStep {
                    parent_id: "agent-1".into(),
                    step: SubagentStep::new(
                        format!("step-{index}"),
                        SubagentStepKind::Message,
                        format!("step {index}"),
                    ),
                },
            )
        }));

        let mut transcript = Transcript::new();
        transcript.extend(&entries);
        let Block::Subagent { steps, .. } = &transcript.blocks()[0] else {
            panic!("expected a delegated-agent row");
        };
        assert_eq!(steps.len(), Transcript::MAX_SUBAGENT_STEPS);
        assert_eq!(steps.first().unwrap().id, "step-1");
        assert_eq!(steps.last().unwrap().id, "step-300");
    }

    #[test]
    fn finishing_without_a_report_stops_the_running_activity() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::SubagentStarted {
                    id: "agent-1".into(),
                    title: "Review correctness".into(),
                },
            ),
            agent(
                2,
                AgentEvent::SubagentFinished {
                    id: "agent-1".into(),
                    summary: None,
                    failed: false,
                },
            ),
        ]);

        assert_eq!(transcript.activity(), Activity::Thinking);
    }

    #[test]
    fn a_result_with_no_call_is_still_shown() {
        // The call may be in a page that has not been fetched; dropping the
        // output would lose it for good.
        let mut transcript = Transcript::new();
        transcript.extend(&[agent(
            1,
            AgentEvent::ToolResult {
                activity: completed("orphan", "surprise", true),
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
                    activity: ActivityItem::from_tool(Some("t".into()), "Bash", &json!({})),
                },
            )
        };
        let result = |seq, output: &str| {
            agent(
                seq,
                AgentEvent::ToolResult {
                    activity: completed("t", output, false),
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
    fn context_usage_stays_out_of_the_conversation_and_keeps_the_latest_reading() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            text(1, "done"),
            agent(
                2,
                AgentEvent::ContextUsage {
                    usage: ContextUsage {
                        used_tokens: 32_000,
                        window_tokens: 128_000,
                        can_compact: true,
                    },
                },
            ),
        ]);

        assert_eq!(transcript.blocks().len(), 1);
        assert_eq!(
            transcript.context_usage(),
            Some(ContextUsage {
                used_tokens: 32_000,
                window_tokens: 128_000,
                can_compact: true,
            })
        );
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
                Block::TurnEnd {
                    turn: 1,
                    seq: 1,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    service_tier: None,
                },
                Block::Outcome {
                    state: SessionState::Finished,
                    summary: Some("done".into()),
                },
            ]
        );
    }

    #[test]
    fn each_turn_keeps_the_provider_and_model_that_actually_started_it() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::TurnStarted {
                    provider: Some("claude".into()),
                    model: Some("sonnet".into()),
                    reasoning_effort: None,
                    service_tier: None,
                },
            ),
            agent(
                2,
                AgentEvent::Connected {
                    session_id: Some("vendor-1".into()),
                    model: Some("claude-sonnet-4-6".into()),
                },
            ),
            agent(3, AgentEvent::TurnEnd { turn: 1 }),
            agent(
                4,
                AgentEvent::TurnStarted {
                    provider: Some("codex".into()),
                    model: Some("gpt-5.4".into()),
                    reasoning_effort: Some("high".into()),
                    service_tier: Some("fast".into()),
                },
            ),
            agent(5, AgentEvent::TurnEnd { turn: 2 }),
        ]);

        assert_eq!(
            transcript.blocks(),
            &[
                Block::TurnEnd {
                    turn: 1,
                    seq: 3,
                    provider: Some("claude".into()),
                    model: Some("claude-sonnet-4-6".into()),
                    reasoning_effort: None,
                    service_tier: None,
                },
                Block::TurnEnd {
                    turn: 2,
                    seq: 5,
                    provider: Some("codex".into()),
                    model: Some("gpt-5.4".into()),
                    reasoning_effort: Some("high".into()),
                    service_tier: Some("fast".into()),
                },
            ]
        );
        assert_eq!(
            turn_provenance(Some("codex"), Some("gpt-5.4"), Some("high"), Some("fast")).as_deref(),
            Some("codex · gpt-5.4 · high · fast")
        );
    }

    #[test]
    fn a_turn_boundary_keeps_the_transcript_position_used_by_a_fork() {
        let mut transcript = Transcript::new();
        transcript.apply(&agent(1, AgentEvent::TurnEnd { turn: 3 }));

        assert_eq!(
            transcript.blocks(),
            &[Block::TurnEnd {
                turn: 3,
                seq: 1,
                provider: None,
                model: None,
                reasoning_effort: None,
                service_tier: None,
            }]
        );
    }

    #[test]
    fn stored_positions_map_to_the_drawable_block_that_folded_them() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            text(1, "hel"),
            text(2, "lo"),
            user(3, "question"),
            agent(
                4,
                AgentEvent::ToolCall {
                    activity: ActivityItem::from_tool(Some("t".into()), "Bash", &json!({})),
                },
            ),
            agent(
                5,
                AgentEvent::ToolResult {
                    activity: completed("t", "answer", false),
                },
            ),
        ]);

        assert_eq!(transcript.block_index_for_seq(1), Some(0));
        assert_eq!(transcript.block_index_for_seq(2), Some(0));
        assert_eq!(transcript.block_index_for_seq(3), Some(1));
        assert_eq!(transcript.block_index_for_seq(5), Some(2));
        assert_eq!(transcript.block_index_for_seq(99), None);
    }

    #[test]
    fn the_prompt_outline_lists_prompts_but_not_interaction_answers() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            user(1, "First prompt"),
            text(2, "Working"),
            TranscriptEntry {
                seq: 3,
                at: 0,
                payload: TranscriptPayload::Response {
                    request_id: "database".into(),
                    text: "SQLite".into(),
                },
            },
            user(4, "Second prompt"),
        ]);

        assert_eq!(
            transcript.prompt_outline().collect::<Vec<_>>(),
            vec![(0, "First prompt"), (3, "Second prompt")]
        );
    }

    #[test]
    fn a_prompt_outline_label_is_one_bounded_line() {
        assert_eq!(
            prompt_outline_label("  Fix the tests\nthen format  ", 80),
            "Fix the tests then format"
        );
        assert_eq!(prompt_outline_label("abcdefgh", 6), "abcde…");
        assert_eq!(prompt_outline_label("abc", 0), "");
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
                activity: ActivityItem::from_tool(Some("t".into()), "Bash", &json!({})),
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
                activity: completed("t", "done", false),
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
    fn only_finished_blocks_are_handed_to_the_formatter() {
        // Half a markdown document is not a markdown document: formatting a
        // heading with no line after it re-flows the text under the reader.
        let (formatted, writing) = settled("# Title\n\nA finished paragraph.\n\n## Half a hea");
        assert_eq!(formatted, "# Title\n\nA finished paragraph.\n\n");
        assert_eq!(writing, "## Half a hea");
    }

    #[test]
    fn the_cut_never_lands_inside_a_fence() {
        // A blank line does not end a fenced block, and formatting one that is
        // still open turns the rest of the answer into code.
        let text = "Here:\n\n```rust\nfn main() {\n\n    println!(\"hi\");\n";
        let (formatted, writing) = settled(text);
        assert_eq!(formatted, "Here:\n\n");
        assert!(writing.starts_with("```rust"));
    }

    #[test]
    fn text_with_nothing_finished_is_all_still_being_written() {
        let (formatted, writing) = settled("just started");
        assert_eq!(formatted, "");
        assert_eq!(writing, "just started");
    }

    #[test]
    fn a_reader_who_scrolled_away_is_not_dragged_back() {
        // The answer growing moves the foot away from them too; a rule that
        // could not tell that apart would pull them off the paragraph they
        // went to read.
        assert_eq!(following(true, false, false), (false, false));
        // Until they come back to the foot themselves.
        assert_eq!(following(true, false, true), (true, true));
    }

    #[test]
    fn a_reader_at_the_foot_is_kept_there_while_the_agent_writes() {
        assert_eq!(following(true, true, false), (true, true));
        // Nothing is being written, so nothing needs pulling.
        assert_eq!(following(false, true, true), (false, true));
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
    fn structured_questions_are_open_until_answered() {
        use ginka_protocol::question::{Choice, Question};
        let questions = vec![Question {
            header: Some("DB".into()),
            question: "Which database?".into(),
            options: vec![Choice {
                label: "SQLite".into(),
                description: Some("local".into()),
            }],
            multi_select: false,
        }];
        let mut transcript = Transcript::new();
        transcript.extend(&[agent(
            1,
            AgentEvent::AskUser {
                id: "ask-2".into(),
                question: "Which database?".into(),
                options: vec!["SQLite".into()],
                questions: questions.clone(),
            },
        )]);
        assert_eq!(
            transcript.open_questions(),
            Some(("ask-2", questions.as_slice()))
        );
        transcript.answer("ask-2");
        assert_eq!(transcript.open_questions(), None);
    }

    #[test]
    fn a_question_carries_what_an_answer_is_sent_against() {
        // Without the id there is nothing to answer to, and the card is a
        // paragraph with buttons that do nothing.
        let mut transcript = Transcript::new();
        transcript.extend(&[agent(
            1,
            AgentEvent::AskUser {
                id: "ask-1".into(),
                question: "Which one?".into(),
                options: vec!["this".into(), "that".into()],
                questions: Vec::new(),
            },
        )]);
        assert_eq!(
            transcript.open_questions(),
            None,
            "a plain question is answered with buttons, not the form"
        );
        match transcript.blocks().last().unwrap() {
            Block::Question {
                id,
                options,
                answered,
                ..
            } => {
                assert_eq!(id, "ask-1");
                assert_eq!(options.len(), 2);
                assert!(!answered, "nobody has said anything yet");
            }
            other => panic!("expected a question, got {other:?}"),
        }
    }

    #[test]
    fn an_ordinary_follow_up_does_not_resolve_an_addressed_request() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            agent(
                1,
                AgentEvent::PlanProposal {
                    id: "plan-1".into(),
                    plan: "do the thing".into(),
                },
            ),
            user(2, "go ahead"),
        ]);
        let answered = transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::Plan { answered: true, .. }));
        assert!(!answered, "{:?}", transcript.blocks());
    }
}

/// What the composer is completing, read from the text before the caret.
///
/// A mention is `@` followed by anything that is not a space, and it only
/// counts at the end of what has been typed: `@src/main.rs and now what?` is a
/// finished mention in a sentence, not a picker that should still be open.
pub fn mention_being_typed(text: &str) -> Option<&str> {
    let last_line = text.rsplit('\n').next()?;
    let at = last_line.rfind('@')?;
    // `foo@bar` is an address, not a mention: one has to start a word.
    let starts_a_word = at == 0
        || last_line[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
    if !starts_a_word {
        return None;
    }
    let query = &last_line[at + 1..];
    (!query.contains(char::is_whitespace)).then_some(query)
}

/// What command is being typed, if the prompt is one.
///
/// Only at the very start: `/review` is a command, and "see /usr/bin for the
/// path" is a sentence about a directory. A command takes the whole prompt, so
/// there is nothing before it to check.
pub fn command_being_typed(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// Replace the command being typed with `name`.
pub fn complete_command(text: &str, name: &str) -> String {
    if command_being_typed(text).is_none() {
        return text.to_string();
    }
    format!("/{name} ")
}

/// Replace the mention being typed with `path`, and say what the text becomes.
///
/// The trailing space is deliberate: a mention is finished once it is chosen,
/// and the next thing typed is a sentence rather than more of the path.
pub fn complete_mention(text: &str, path: &str) -> String {
    let Some(query) = mention_being_typed(text) else {
        return text.to_string();
    };
    let cut = text.len() - query.len();
    format!("{}{path} ", &text[..cut])
}

#[cfg(test)]
mod mentions {
    use super::*;

    #[test]
    fn an_at_sign_starts_a_mention() {
        assert_eq!(mention_being_typed("look at @src/ma"), Some("src/ma"));
        assert_eq!(mention_being_typed("@"), Some(""));
    }

    #[test]
    fn a_finished_mention_is_not_still_being_typed() {
        // Otherwise the picker stays open over the rest of the sentence.
        assert_eq!(mention_being_typed("@src/main.rs and then"), None);
    }

    #[test]
    fn an_address_is_not_a_mention() {
        assert_eq!(mention_being_typed("mail me at bob@example"), None);
    }

    #[test]
    fn nothing_typed_is_not_a_mention() {
        assert_eq!(mention_being_typed(""), None);
        assert_eq!(mention_being_typed("no at sign here"), None);
    }

    #[test]
    fn only_the_line_being_typed_counts() {
        assert_eq!(mention_being_typed("@old/path\nand now"), None);
        assert_eq!(mention_being_typed("first line\n@sec"), Some("sec"));
    }

    #[test]
    fn choosing_a_file_finishes_the_mention() {
        assert_eq!(
            complete_mention("look at @src/ma", "src/main.rs"),
            "look at @src/main.rs ",
            "a chosen mention is finished, and what follows is a sentence"
        );
        assert_eq!(complete_mention("@", "README.md"), "@README.md ");
    }

    #[test]
    fn a_slash_at_the_start_is_a_command() {
        assert_eq!(command_being_typed("/rev"), Some("rev"));
        assert_eq!(command_being_typed("/"), Some(""));
    }

    #[test]
    fn a_slash_anywhere_else_is_a_path() {
        // "see /usr/bin for the path" is a sentence, not a command.
        assert_eq!(command_being_typed("see /usr/bin"), None);
        // And a command with an argument is no longer being chosen.
        assert_eq!(command_being_typed("/review src/main.rs"), None);
    }

    #[test]
    fn choosing_a_command_replaces_what_was_typed() {
        assert_eq!(complete_command("/rev", "review"), "/review ");
        assert_eq!(complete_command("not a command", "review"), "not a command");
    }

    #[test]
    fn completing_when_nothing_is_being_typed_changes_nothing() {
        assert_eq!(complete_mention("plain text", "x.rs"), "plain text");
    }

    #[test]
    fn a_turn_says_how_long_it_took_and_when_it_ended() {
        let mut transcript = Transcript::new();
        transcript.extend(&[
            TranscriptEntry {
                seq: 1,
                at: 100,
                payload: TranscriptPayload::User { text: "hi".into() },
            },
            TranscriptEntry {
                seq: 2,
                at: 103,
                payload: TranscriptPayload::Agent {
                    event: AgentEvent::TurnEnd { turn: 1 },
                },
            },
            TranscriptEntry {
                seq: 3,
                at: 200,
                payload: TranscriptPayload::Agent {
                    event: AgentEvent::TurnEnd { turn: 2 },
                },
            },
        ]);
        assert_eq!(
            transcript.turn_timing(1),
            Some(TurnTiming {
                seconds: Some(3),
                ended_at: 103
            })
        );
        // A second end with no prompt of its own says when, not how long.
        assert_eq!(transcript.turn_timing(2).and_then(|t| t.seconds), None);
        assert_eq!(transcript.turn_timing(0), None);
    }

    #[test]
    fn a_duration_reads_at_the_scale_it_is() {
        assert_eq!(duration_label(3), "3s");
        assert_eq!(duration_label(60), "1m");
        assert_eq!(duration_label(125), "2m 5s");
        assert_eq!(duration_label(3840), "1h 4m");
    }
}

#[cfg(test)]
mod checkpoint_selection_tests {
    use super::checkpoint_for_turn;
    use ginka_protocol::{Checkpoint, CheckpointId, SessionId, WorkspaceId};

    #[test]
    fn a_turn_action_cannot_restore_another_conversation_or_a_safety_snapshot() {
        let point = |session: &str, completed| Checkpoint {
            id: CheckpointId(session.into()),
            session: SessionId(session.into()),
            workspace: WorkspaceId("project/workspace".into()),
            turn: 1,
            commit: "commit".into(),
            label: "turn".into(),
            has_turn_start: completed,
            can_undo: completed,
            created_at: 0,
        };
        let points = vec![
            point("other", true),
            point("current", false),
            point("current", true),
        ];
        let session = SessionId("current".into());
        assert_eq!(checkpoint_for_turn(&points, &session, 1), Some(&points[2]));
        assert!(checkpoint_for_turn(&points, &session, 2).is_none());
    }
}
