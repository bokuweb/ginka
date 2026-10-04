//! Structured questions an agent asks, and the answers that go back.
//!
//! Claude's `AskUserQuestion` and Codex's `requestUserInput` ask one or more
//! questions, each with labelled choices that carry a description, some
//! allowing several picks. [`Question`] keeps that shape on the wire so a
//! window can draw it as a form. The reply still travels as the plain string
//! `RespondToAgent` has always taken: [`Answers::encode`] writes it, and a
//! driver reads it back with [`Answers::decode`] — falling back to taking the
//! whole string as the answer to every question, which is what the CLI, MCP
//! and Slack send when they answer in words.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One question in an agent's request.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// A short tag the agent gave it ("DB", "Auth"), when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// What the agent wants to know. Answers are keyed by this text.
    pub question: String,
    /// The choices offered, in the agent's order; empty for a free-text
    /// question.
    #[serde(default)]
    pub options: Vec<Choice>,
    /// Whether more than one choice may be picked.
    #[serde(default)]
    pub multi_select: bool,
}

/// One choice a [`Question`] offers.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// What picking it answers with.
    pub label: String,
    /// What it means, in the agent's words, when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The reader's answers to a card's questions, keyed by each question's text.
///
/// A question answered with nothing — or every question, when the reader
/// skipped the card — maps to an empty list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answers {
    /// The picks and typed answers for each question, in the order given.
    pub answers: BTreeMap<String, Vec<String>>,
}

impl Answers {
    /// Answers that skip every one of `questions`.
    pub fn skipped(questions: &[Question]) -> Self {
        Self {
            answers: questions
                .iter()
                .map(|question| (question.question.clone(), Vec::new()))
                .collect(),
        }
    }

    /// Whether nothing at all was answered: the card was skipped.
    pub fn is_skip(&self) -> bool {
        self.answers.values().all(Vec::is_empty)
    }

    /// The `RespondToAgent` response that carries these answers.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Read a response back: structured answers when it is one
    /// [`Answers::encode`] wrote, `None` for an answer given in words.
    pub fn decode(response: &str) -> Option<Self> {
        let trimmed = response.trim();
        if !trimmed.starts_with('{') {
            return None;
        }
        serde_json::from_str(trimmed).ok()
    }

    /// The answer to `question` as one line — picks joined with ", " — or
    /// `None` when it was left unanswered.
    pub fn line(&self, question: &str) -> Option<String> {
        self.answers
            .get(question)
            .filter(|picked| !picked.is_empty())
            .map(|picked| picked.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(text: &str) -> Question {
        Question {
            header: None,
            question: text.into(),
            options: vec![
                Choice {
                    label: "SQLite".into(),
                    description: Some("local file".into()),
                },
                Choice {
                    label: "Postgres".into(),
                    description: None,
                },
            ],
            multi_select: false,
        }
    }

    #[test]
    fn answers_survive_the_response_string() {
        let mut answers = Answers::default();
        answers
            .answers
            .insert("Which database?".into(), vec!["SQLite".into()]);
        answers.answers.insert(
            "Which features?".into(),
            vec!["Auth".into(), "a typed answer".into()],
        );
        let decoded = Answers::decode(&answers.encode()).expect("structured");
        assert_eq!(decoded, answers);
        assert_eq!(decoded.line("Which database?").as_deref(), Some("SQLite"));
        assert_eq!(
            decoded.line("Which features?").as_deref(),
            Some("Auth, a typed answer")
        );
        assert!(!decoded.is_skip());
    }

    #[test]
    fn words_are_not_structured_answers() {
        assert_eq!(Answers::decode("SQLite"), None);
        assert_eq!(Answers::decode("  {not json"), None);
    }

    #[test]
    fn a_skipped_card_answers_nothing() {
        let skipped = Answers::skipped(&[question("Which database?")]);
        assert!(skipped.is_skip());
        assert_eq!(skipped.line("Which database?"), None);
        assert_eq!(Answers::decode(&skipped.encode()), Some(skipped));
    }

    #[test]
    fn a_question_without_the_new_fields_still_reads() {
        let read: Question = serde_json::from_str(r#"{"question":"Why?"}"#).unwrap();
        assert!(read.options.is_empty());
        assert!(!read.multi_select);
        assert_eq!(read.header, None);
    }
}
