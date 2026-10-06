//! The form an agent's structured questions are answered with.
//!
//! Claude's `AskUserQuestion` and Codex's `requestUserInput` ask one or more
//! [`Question`]s, each with described choices, some taking several picks. The
//! card draws one row per choice, numbered so a key picks it, an *Other* field
//! for an answer the agent did not offer, and *Skip* and *Send*. This holds
//! what the reader has picked and typed while revisiting questions, and turns
//! it into the [`Answers`] the driver sends back.

use ginka_protocol::question::{Answers, Question};
use std::collections::BTreeSet;

/// What the reader has picked and typed so far on one card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionForm {
    questions: Vec<Question>,
    /// The picked choices of each question, by index into its options.
    picked: Vec<BTreeSet<usize>>,
    /// What was typed into each question's *Other* field.
    other: Vec<String>,
    /// The question number keys pick in.
    current: usize,
}

/// What a number key did on the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// It picked (or, for several picks, toggled) a choice.
    Picked,
    /// It is the number after the last choice: the *Other* field wants focus.
    Other,
    /// It means nothing on this question.
    Ignored,
}

impl QuestionForm {
    /// A blank form for `questions`, with the first one current.
    pub fn new(questions: Vec<Question>) -> Self {
        let count = questions.len();
        Self {
            questions,
            picked: vec![BTreeSet::new(); count],
            other: vec![String::new(); count],
            current: 0,
        }
    }

    /// The questions the form answers.
    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    /// The question number keys pick in.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Make `question` the one number keys pick in.
    pub fn focus(&mut self, question: usize) {
        if question < self.questions.len() {
            self.current = question;
        }
    }

    /// Whether an earlier question can be revisited.
    pub fn can_go_back(&self) -> bool {
        self.current > 0
    }

    /// Whether a later question exists, even if this one is unanswered.
    pub fn can_go_next(&self) -> bool {
        self.current + 1 < self.questions.len()
    }

    /// Revisit the previous question without changing any answers; never wraps.
    /// Returns whether the current question changed.
    pub fn go_back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.current -= 1;
        true
    }

    /// Visit the next question without changing any answers; never wraps.
    /// Returns whether the current question changed. Sending still requires
    /// every question to be answered.
    pub fn go_next(&mut self) -> bool {
        if !self.can_go_next() {
            return false;
        }
        self.current += 1;
        true
    }

    /// Pick `choice` in `question`. With one pick allowed it replaces the
    /// earlier pick and clears *Other*; with several it toggles.
    pub fn pick(&mut self, question: usize, choice: usize) {
        let Some(asked) = self.questions.get(question) else {
            return;
        };
        if choice >= asked.options.len() {
            return;
        }
        let picked = &mut self.picked[question];
        if asked.multi_select {
            if !picked.remove(&choice) {
                picked.insert(choice);
            }
        } else {
            picked.clear();
            picked.insert(choice);
            self.other[question].clear();
        }
        self.current = question;
    }

    /// Whether `choice` is picked in `question`.
    pub fn is_picked(&self, question: usize, choice: usize) -> bool {
        self.picked
            .get(question)
            .is_some_and(|picked| picked.contains(&choice))
    }

    /// Keep what was typed into `question`'s *Other* field. With one pick
    /// allowed, typing an answer of one's own replaces the pick.
    pub fn set_other(&mut self, question: usize, text: &str) {
        let Some(asked) = self.questions.get(question) else {
            return;
        };
        if !asked.multi_select && !text.trim().is_empty() {
            self.picked[question].clear();
        }
        self.other[question] = text.to_string();
    }

    /// What `question`'s *Other* field holds.
    pub fn other(&self, question: usize) -> &str {
        self.other.get(question).map(String::as_str).unwrap_or("")
    }

    /// Number key `number` (1-based) on the current question.
    pub fn press_number(&mut self, number: usize) -> KeyOutcome {
        let Some(asked) = self.questions.get(self.current) else {
            return KeyOutcome::Ignored;
        };
        let count = asked.options.len();
        match number {
            0 => KeyOutcome::Ignored,
            n if n <= count => {
                self.pick(self.current, n - 1);
                KeyOutcome::Picked
            }
            n if n == count + 1 => KeyOutcome::Other,
            _ => KeyOutcome::Ignored,
        }
    }

    /// Whether `question` has an answer: a pick, or words of its own.
    pub fn is_answered(&self, question: usize) -> bool {
        !self.picked[question].is_empty() || !self.other[question].trim().is_empty()
    }

    /// Whether every question has an answer, so the form can be sent.
    pub fn is_complete(&self) -> bool {
        (0..self.questions.len()).all(|question| self.is_answered(question))
    }

    /// The answers to send: each question's picks in the agent's order, then
    /// its *Other* words.
    pub fn answers(&self) -> Answers {
        let mut answers = Answers::default();
        for (index, question) in self.questions.iter().enumerate() {
            let mut given: Vec<String> = self.picked[index]
                .iter()
                .filter_map(|&choice| question.options.get(choice))
                .map(|choice| choice.label.clone())
                .collect();
            let typed = self.other[index].trim();
            if !typed.is_empty() {
                given.push(typed.to_string());
            }
            answers.answers.insert(question.question.clone(), given);
        }
        answers
    }

    /// The answers that skip the whole card.
    pub fn skip(&self) -> Answers {
        Answers::skipped(&self.questions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::question::Choice;

    fn question(text: &str, labels: &[&str], multi_select: bool) -> Question {
        Question {
            header: None,
            question: text.into(),
            options: labels
                .iter()
                .map(|label| Choice {
                    label: (*label).into(),
                    description: None,
                })
                .collect(),
            multi_select,
        }
    }

    #[test]
    fn one_pick_replaces_the_last_and_typing_replaces_the_pick() {
        let mut form = QuestionForm::new(vec![question("DB?", &["SQLite", "Postgres"], false)]);
        assert!(!form.is_complete());
        form.pick(0, 0);
        form.pick(0, 1);
        assert!(!form.is_picked(0, 0) && form.is_picked(0, 1));
        assert!(form.is_complete());

        form.set_other(0, "DuckDB");
        assert!(!form.is_picked(0, 1), "words of one's own replace the pick");
        assert_eq!(form.answers().line("DB?").as_deref(), Some("DuckDB"));

        form.pick(0, 0);
        assert_eq!(form.other(0), "", "a pick replaces the words");
        assert_eq!(form.answers().line("DB?").as_deref(), Some("SQLite"));
    }

    #[test]
    fn several_picks_toggle_and_keep_the_agents_order_then_the_words() {
        let mut form = QuestionForm::new(vec![question(
            "Features?",
            &["Auth", "Billing", "Search"],
            true,
        )]);
        form.pick(0, 2);
        form.pick(0, 0);
        form.pick(0, 1);
        form.pick(0, 1);
        form.set_other(0, "audit logs");
        assert_eq!(
            form.answers().answers["Features?"],
            vec!["Auth", "Search", "audit logs"]
        );
    }

    #[test]
    fn number_keys_pick_in_the_current_question_and_the_next_number_is_other() {
        let mut form = QuestionForm::new(vec![
            question("DB?", &["SQLite", "Postgres"], false),
            question("Name?", &[], false),
        ]);
        assert_eq!(form.press_number(2), KeyOutcome::Picked);
        assert!(form.is_picked(0, 1));
        assert_eq!(form.press_number(3), KeyOutcome::Other);
        assert_eq!(form.press_number(4), KeyOutcome::Ignored);
        assert_eq!(form.press_number(0), KeyOutcome::Ignored);

        form.focus(1);
        assert_eq!(form.press_number(1), KeyOutcome::Other, "free text only");
        assert!(!form.is_complete(), "the second question is unanswered");
        form.set_other(1, "ledger");
        assert!(form.is_complete());
    }

    #[test]
    fn navigation_is_bounded_even_for_empty_and_single_question_forms() {
        for questions in [vec![], vec![question("Name?", &[], false)]] {
            let mut form = QuestionForm::new(questions);
            assert!(!form.can_go_back());
            assert!(!form.can_go_next());
            assert!(!form.go_back());
            assert!(!form.go_next());
            form.focus(usize::MAX);
            assert_eq!(form.current(), 0);
        }
        let mut form = QuestionForm::new(vec![
            question("DB?", &["SQLite"], false),
            question("Name?", &[], false),
        ]);
        assert!(form.can_go_next());
        assert!(form.go_next());
        assert_eq!(form.current(), 1);
        assert!(!form.go_next());
        assert!(form.can_go_back());
        assert!(form.go_back());
        assert_eq!(form.current(), 0);
        assert!(!form.go_back());
        assert!(!form.is_complete(), "navigation does not answer questions");
    }

    #[test]
    fn revisiting_and_editing_an_answer_preserves_the_other_questions() {
        let mut form = QuestionForm::new(vec![
            question("DB?", &["SQLite", "Postgres"], false),
            question("Name?", &[], false),
            question("Features?", &["Auth", "Search"], true),
        ]);
        form.press_number(1);
        form.go_next();
        form.set_other(1, "  ledger  ");
        form.go_next();
        form.press_number(2);
        form.set_other(2, "audit logs");
        let before = form.answers();
        assert!(form.is_complete());

        form.go_back();
        assert_eq!(form.other(1), "  ledger  ");
        form.go_back();
        assert!(form.is_picked(0, 0));
        assert_eq!(form.answers(), before, "moving never changes an answer");
        form.press_number(2);
        form.go_next();
        form.set_other(1, "   ");
        assert!(
            !form.is_complete(),
            "clearing an earlier answer blocks send"
        );
        form.set_other(1, "journal");
        form.go_next();
        assert!(form.is_picked(2, 1));
        assert_eq!(form.other(2), "audit logs");
        assert!(form.is_complete());
        assert_eq!(form.answers().answers["DB?"], vec!["Postgres"]);
        assert_eq!(form.answers().answers["Name?"], vec!["journal"]);
        assert_eq!(
            form.answers().answers["Features?"],
            vec!["Search", "audit logs"]
        );
    }

    #[test]
    fn skipping_answers_nothing_to_every_question() {
        let form = QuestionForm::new(vec![
            question("DB?", &["SQLite"], false),
            question("Name?", &[], false),
        ]);
        let skipped = form.skip();
        assert!(skipped.is_skip());
        assert_eq!(skipped.answers.len(), 2);
    }
}
