//! Moving a conversation to another agent.
//!
//! A fork onto a different provider, or onto a different login of the same
//! one, cannot continue the vendor's thread: it lives in the vendor's own
//! store, in the account's directory, and `resume` cannot cross either
//! (`docs/roadmap.md` §3.3 N2, `docs/accounts.md` §5). What *can* cross is the
//! record. The daemon keeps every transcript in one normalized shape whatever
//! produced it (rule 6), so a digest of one agent's conversation can be
//! written for another to read.
//!
//! The digest is addressed to the agent that takes over, and prepended to its
//! first prompt. It is bounded, because a long conversation would otherwise
//! spend the new agent's context window before it has done anything: the
//! opening prompt — the task — is always kept, and turns are dropped from the
//! middle, oldest first, because the most recent state of the work is what
//! the next turn is about. The working tree is not described: the agent can
//! read it, and it is the one thing that is already true.

use ginka_protocol::event::{ActivityKind, AgentEvent};
use ginka_protocol::model::{TranscriptEntry, TranscriptPayload};
use std::collections::BTreeSet;

/// How much of a digest an agent is handed, in characters.
///
/// A few thousand tokens: enough for the task, the shape of the work and the
/// last exchange, and small beside any provider's context window.
pub const DEFAULT_BUDGET: usize = 12_000;

/// What one agent reply is trimmed to inside the digest. The head carries the
/// plan and the tail carries the conclusion; the middle is the working.
const REPLY_HEAD: usize = 1_200;
const REPLY_TAIL: usize = 600;

/// One exchange: what was asked, what the agent answered, what it touched.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Turn {
    asked: Vec<String>,
    answered: String,
    files: BTreeSet<String>,
    commands: Vec<String>,
}

impl Turn {
    fn is_empty(&self) -> bool {
        self.asked.is_empty()
            && self.answered.trim().is_empty()
            && self.files.is_empty()
            && self.commands.is_empty()
    }
}

/// Fold a transcript into turns: a user message opens one, and everything the
/// agent did until the next user message belongs to it.
fn fold(entries: &[TranscriptEntry]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for entry in entries {
        match &entry.payload {
            TranscriptPayload::User { text } => {
                // Two prompts with nothing between them are one thought — a
                // follow-up typed before the agent answered the first.
                match turns.last_mut() {
                    Some(turn) if turn.answered.trim().is_empty() && turn.files.is_empty() => {
                        turn.asked.push(text.clone());
                    }
                    _ => turns.push(Turn {
                        asked: vec![text.clone()],
                        ..Turn::default()
                    }),
                }
            }
            TranscriptPayload::Response { text, .. } => match turns.last_mut() {
                Some(turn) => turn.asked.push(text.clone()),
                None => turns.push(Turn {
                    asked: vec![text.clone()],
                    ..Turn::default()
                }),
            },
            TranscriptPayload::Agent { event } => {
                let turn = match turns.last_mut() {
                    Some(turn) => turn,
                    None => {
                        turns.push(Turn::default());
                        turns.last_mut().expect("just pushed")
                    }
                };
                match event {
                    AgentEvent::TextDelta { text } => turn.answered.push_str(text),
                    AgentEvent::ToolCall { activity } | AgentEvent::ToolResult { activity } => {
                        match activity.kind {
                            ActivityKind::FileChange => {
                                turn.files.insert(activity.title.clone());
                            }
                            ActivityKind::Command if !turn.commands.contains(&activity.title) => {
                                turn.commands.push(activity.title.clone());
                            }
                            _ => {}
                        }
                    }
                    // Reasoning is the previous agent's, not a fact about the
                    // work; questions and plans were answered by what came
                    // after them.
                    _ => {}
                }
            }
        }
    }
    turns.retain(|turn| !turn.is_empty());
    turns
}

/// Keep `head` characters from the front and `tail` from the back.
fn trim_middle(text: &str, head: usize, tail: usize) -> String {
    let count = text.chars().count();
    if count <= head + tail {
        return text.to_string();
    }
    let front: String = text.chars().take(head).collect();
    let back: String = text.chars().skip(count - tail).collect();
    format!(
        "{front}\n[… {} characters omitted …]\n{back}",
        count - head - tail
    )
}

fn render_turn(turn: &Turn, number: usize, agent: &str) -> String {
    let mut out = format!("## Turn {number}\n");
    for asked in &turn.asked {
        out.push_str("User: ");
        out.push_str(asked.trim());
        out.push('\n');
    }
    let answered = turn.answered.trim();
    if !answered.is_empty() {
        out.push_str(agent);
        out.push_str(": ");
        out.push_str(&trim_middle(answered, REPLY_HEAD, REPLY_TAIL));
        out.push('\n');
    }
    if !turn.commands.is_empty() {
        out.push_str("Ran: ");
        out.push_str(&turn.commands.join("; "));
        out.push('\n');
    }
    if !turn.files.is_empty() {
        out.push_str("Files changed: ");
        out.push_str(&turn.files.iter().cloned().collect::<Vec<_>>().join(", "));
        out.push('\n');
    }
    out
}

fn preface(agent: &str) -> String {
    format!(
        "This conversation was moved to you from another agent ({agent}). What follows is a \
         digest of it, oldest first. The working tree already holds the work described; read \
         it rather than redoing it, and continue from the last turn.\n\n"
    )
}

/// The digest an agent is handed when a conversation is moved to it.
///
/// `agent` names who the transcript came from, so the new agent can tell its
/// predecessor's words from the user's. The result is at most about `budget`
/// characters: the first turn is always present, and when the rest does not
/// fit, the turns between it and the most recent ones are replaced by one
/// line saying how many were left out. An empty transcript gives an empty
/// digest — there is nothing to hand over.
pub fn digest(entries: &[TranscriptEntry], agent: &str, budget: usize) -> String {
    let turns = fold(entries);
    if turns.is_empty() {
        return String::new();
    }
    let rendered: Vec<String> = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| render_turn(turn, index + 1, agent))
        .collect();

    let preface = preface(agent);
    let total = |kept: &[&str]| -> usize {
        preface.len() + kept.iter().map(|s| s.len() + 1).sum::<usize>()
    };

    // Drop from just after the first turn until the tail fits.
    let mut omitted = 0;
    let mut kept: Vec<&str> = rendered.iter().map(String::as_str).collect();
    while total(&kept) > budget && kept.len() > 2 {
        kept.remove(1);
        omitted += 1;
    }

    let mut out = preface;
    let mut turns_out = kept.into_iter();
    if let Some(first) = turns_out.next() {
        out.push_str(first);
        out.push('\n');
    }
    if omitted > 0 {
        out.push_str(&format!("[… {omitted} earlier turn(s) omitted …]\n\n"));
    }
    for turn in turns_out {
        out.push_str(turn);
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::event::ActivityItem;

    fn user(seq: u64, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            seq,
            at: seq as i64,
            payload: TranscriptPayload::User {
                text: text.to_string(),
            },
        }
    }

    fn said(seq: u64, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            seq,
            at: seq as i64,
            payload: TranscriptPayload::Agent {
                event: AgentEvent::TextDelta {
                    text: text.to_string(),
                },
            },
        }
    }

    fn edited(seq: u64, path: &str) -> TranscriptEntry {
        TranscriptEntry {
            seq,
            at: seq as i64,
            payload: TranscriptPayload::Agent {
                event: AgentEvent::ToolResult {
                    activity: ActivityItem {
                        id: None,
                        kind: ActivityKind::FileChange,
                        title: path.to_string(),
                        tasks: None,
                        detail: None,
                        failed: false,
                        complete: true,
                    },
                },
            },
        }
    }

    #[test]
    fn a_digest_says_who_asked_who_answered_and_what_was_touched() {
        let entries = [
            user(1, "make the parser stricter"),
            said(2, "I tightened "),
            said(3, "the grammar."),
            edited(4, "src/parser.rs"),
            user(5, "and the tests?"),
            said(6, "Added two."),
            edited(7, "tests/parser.rs"),
        ];
        let text = digest(&entries, "claude", DEFAULT_BUDGET);
        assert!(text.contains("from another agent (claude)"), "{text}");
        assert!(text.contains("User: make the parser stricter"), "{text}");
        // Deltas are folded into one reply, attributed to the agent that
        // gave it rather than to the user.
        assert!(text.contains("claude: I tightened the grammar."), "{text}");
        assert!(text.contains("Files changed: src/parser.rs"), "{text}");
        assert!(text.contains("## Turn 2"), "{text}");
        assert!(text.contains("Files changed: tests/parser.rs"), "{text}");
    }

    #[test]
    fn the_task_survives_and_the_middle_goes_when_the_budget_is_short() {
        let mut entries = vec![user(1, "the task itself")];
        for turn in 0..20u64 {
            let base = 2 + turn * 2;
            entries.push(user(base, &format!("follow-up {turn}")));
            entries.push(said(base + 1, &"x".repeat(400)));
        }
        let text = digest(&entries, "claude", 3_000);
        assert!(text.len() <= 3_200, "roughly within budget: {}", text.len());
        assert!(
            text.contains("the task itself"),
            "the opening prompt is never dropped"
        );
        assert!(
            text.contains("follow-up 19"),
            "the latest turn is what comes next"
        );
        assert!(
            !text.contains("follow-up 3\n"),
            "the middle is what goes: {text}"
        );
        assert!(text.contains("earlier turn(s) omitted"), "{text}");
    }

    #[test]
    fn a_long_reply_keeps_its_plan_and_its_conclusion() {
        let reply = format!(
            "{}{}{}",
            "plan ".repeat(300),
            "work ".repeat(500),
            "done ".repeat(200)
        );
        let entries = [user(1, "go"), said(2, &reply)];
        let text = digest(&entries, "codex", DEFAULT_BUDGET);
        assert!(text.contains("plan plan"), "{text}");
        assert!(text.contains("done done"), "{text}");
        assert!(text.contains("characters omitted"), "{text}");
    }

    #[test]
    fn two_prompts_with_nothing_between_them_are_one_turn() {
        let entries = [user(1, "first"), user(2, "and also"), said(3, "ok")];
        let text = digest(&entries, "claude", DEFAULT_BUDGET);
        assert!(text.contains("## Turn 1"), "{text}");
        assert!(!text.contains("## Turn 2"), "{text}");
    }

    #[test]
    fn nothing_to_hand_over_is_an_empty_digest() {
        assert_eq!(digest(&[], "claude", DEFAULT_BUDGET), "");
    }
}
