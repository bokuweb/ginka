//! Session identity as it is shown to the user.
//!
//! A session carries two names: one the user typed and one the agent supplied.
//! They are separate fields rather than one because they have different owners
//! — an agent that renames a session the user already named is a bug, and the
//! only way to be sure it cannot happen is to never write to the same slot.
//! See `docs/roadmap.md` §3.3 N5.

use serde::{Deserialize, Serialize};

/// Shown until anything better exists.
pub const DEFAULT_TITLE: &str = "New session";

/// A session's title, kept as the user's and the agent's separate names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTitle {
    /// What the user typed. Wins whenever it is set.
    user: Option<String>,
    /// What the agent called this session, or the placeholder below.
    agent: Option<String>,
    /// True while `agent` holds prompt text rather than a real title, so a
    /// provider title can replace it and we can tell we are still waiting.
    agent_is_placeholder: bool,
}

impl SessionTitle {
    /// Words of the first prompt the placeholder keeps.
    pub const MAX_PLACEHOLDER_WORDS: usize = 7;
    /// Characters the placeholder keeps, for prompts that do not use spaces.
    pub const MAX_PLACEHOLDER_CHARS: usize = 60;

    /// Rebuild from stored columns. The database is the only caller: every
    /// other path goes through the setters, which enforce the precedence.
    pub fn from_parts(
        user: Option<String>,
        agent: Option<String>,
        agent_is_placeholder: bool,
    ) -> Self {
        Self {
            user,
            agent,
            agent_is_placeholder,
        }
    }

    /// What the user typed, for storage.
    pub fn user_title(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// What the agent supplied (or the placeholder), for storage.
    pub fn agent_title(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Whether the agent title is still prompt text standing in for a real one.
    pub fn agent_title_is_placeholder(&self) -> bool {
        self.agent_is_placeholder
    }

    /// What to show: the user's title, else the agent's, else [`DEFAULT_TITLE`].
    pub fn display(&self) -> &str {
        self.user
            .as_deref()
            .or(self.agent.as_deref())
            .unwrap_or(DEFAULT_TITLE)
    }

    /// Set (or, with `None` or blank text, clear) the user's own title.
    pub fn set_user(&mut self, title: Option<String>) {
        self.user = title.and_then(|text| {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
    }

    /// Record the title the agent generated for its own session. Silently
    /// replaces a placeholder, and never touches the user's title.
    pub fn set_agent(&mut self, title: impl Into<String>) {
        let title = title.into();
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return;
        }
        self.agent = Some(trimmed.to_string());
        self.agent_is_placeholder = false;
    }

    /// Fill in prompt text so the sidebar is not full of "New session" while
    /// the agent's own title is still being generated. Returns whether it
    /// seeded anything: it is a no-op once any agent title exists.
    pub fn seed_from_prompt(&mut self, prompt: &str) -> bool {
        if self.agent.is_some() {
            return false;
        }
        let Some(text) = placeholder_from(prompt) else {
            return false;
        };
        self.agent = Some(text);
        self.agent_is_placeholder = true;
        true
    }

    /// Whether a title from the agent would still improve on what we show.
    /// Drivers use this to decide whether a metadata lookup is worth making.
    pub fn wants_agent_title(&self) -> bool {
        self.agent.is_none() || self.agent_is_placeholder
    }
}

fn placeholder_from(prompt: &str) -> Option<String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return None;
    }

    let mut words = prompt.split_whitespace();
    let kept: Vec<&str> = words
        .by_ref()
        .take(SessionTitle::MAX_PLACEHOLDER_WORDS)
        .collect();
    let mut truncated = words.next().is_some();
    let mut text: String = kept.join(" ");

    if text.chars().count() > SessionTitle::MAX_PLACEHOLDER_CHARS {
        text = text
            .chars()
            .take(SessionTitle::MAX_PLACEHOLDER_CHARS)
            .collect();
        truncated = true;
    }

    let text = text.trim_end().to_string();
    if text.is_empty() {
        return None;
    }
    Some(if truncated {
        format!("{text}…")
    } else {
        text
    })
}
