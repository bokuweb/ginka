//! View models for the shell.
//!
//! M0 renders the layout with sample rows so the design in `docs/ui.md` can be
//! judged before the data layer exists. M1 replaces `SessionRow::samples()`
//! with real projects and worktrees read through `ginka-core`; nothing else
//! here should have to change when it does.

use crate::assets::icon;
use gpui::SharedString;
use gpui_component::Icon;

/// Which coding agent owns a session.
///
/// The sidebar marks each row with the agent's glyph rather than its name:
/// at three lines per row there is no space for a word, and the shape is
/// recognisable at 12px where text is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
}

impl Agent {
    pub fn glyph(self) -> Icon {
        Icon::empty().path(match self {
            Self::Claude => icon::AGENT_SPARK,
            Self::Codex => icon::AGENT_ORBIT,
            Self::Gemini => icon::AGENT_CUBE,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
        }
    }
}

/// What an agent is doing, as shown in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// The agent is running.
    Working,
    /// The agent asked a question and is blocked on the user.
    NeedsAttention,
    /// Nothing is running.
    Idle,
}

impl AgentState {
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Working => Some("Working"),
            Self::NeedsAttention => Some("Needs you"),
            Self::Idle => None,
        }
    }
}

/// One row in the session list.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub title: SharedString,
    pub agent: Agent,
    /// `project @ device`, the muted line above the title.
    pub origin: SharedString,
    /// The worktree branch, truncated from the left when it does not fit.
    pub branch: SharedString,
    pub state: AgentState,
    /// Relative time, shown when the row is not running.
    pub age: SharedString,
    pub archived: bool,
}

impl SessionRow {
    #[allow(clippy::too_many_arguments)]
    fn new(
        title: &str,
        origin: &str,
        branch: &str,
        agent: Agent,
        state: AgentState,
        age: &str,
        archived: bool,
    ) -> Self {
        Self {
            title: title.into(),
            agent,
            origin: origin.into(),
            branch: branch.into(),
            state,
            age: age.into(),
            archived,
        }
    }

    /// Placeholder content for M0. Replaced by real data in M1.
    pub fn samples() -> Vec<Self> {
        vec![
            Self::new(
                "Repository Story Creation",
                "ginka @ personal-metal",
                "ginka/repository-story-creation",
                Agent::Gemini,
                AgentState::Idle,
                "now",
                false,
            ),
            Self::new(
                "PR 78 Audit And Review",
                "ginka @ personal-metal",
                "HEAD",
                Agent::Claude,
                AgentState::Working,
                "2m",
                false,
            ),
            Self::new(
                "Remove R2 File Uploads",
                "ginka @ personal-metal",
                "ginka/remove-r2-file-uploads",
                Agent::Codex,
                AgentState::NeedsAttention,
                "9m",
                false,
            ),
            Self::new(
                "Local First",
                "ginka @ personal-metal",
                "ginka/pr-30-silent-audit",
                Agent::Claude,
                AgentState::Idle,
                "46m",
                false,
            ),
            Self::new(
                "Session Done Restart Loop",
                "ginka @ personal-metal",
                "ginka/session-done-restart-loop",
                Agent::Claude,
                AgentState::Idle,
                "4h",
                true,
            ),
            Self::new(
                "Six Hundred Word Story",
                "ginka @ personal-metal",
                "ginka/six-hundred-word-story",
                Agent::Gemini,
                AgentState::Idle,
                "4h",
                true,
            ),
            Self::new(
                "Privatize Orbit Repository",
                "ginka @ personal-metal",
                "ginka/privatize-orbit-repository",
                Agent::Codex,
                AgentState::Idle,
                "6h",
                true,
            ),
        ]
    }

    /// Working first, then rows waiting on the user, then everything else.
    ///
    /// The reorder animates on the standard curve (`docs/ui.md` §3.2). Sorting
    /// is stable so equal rows keep their relative order and nothing jumps
    /// under the cursor for no reason.
    pub fn attention_rank(&self) -> u8 {
        match self.state {
            AgentState::Working => 0,
            AgentState::NeedsAttention => 1,
            AgentState::Idle => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_sort_puts_running_work_first_and_is_stable() {
        let mut rows = SessionRow::samples();
        rows.sort_by_key(SessionRow::attention_rank);
        assert_eq!(rows[0].state, AgentState::Working);
        assert_eq!(rows[1].state, AgentState::NeedsAttention);
        // Idle rows keep the order they were given.
        let idle: Vec<_> = rows
            .iter()
            .filter(|r| r.state == AgentState::Idle)
            .collect();
        assert_eq!(idle[0].title, "Repository Story Creation");
        assert_eq!(idle[1].title, "Local First");
    }

    #[test]
    fn idle_rows_show_a_timestamp_rather_than_a_status_word() {
        assert_eq!(AgentState::Idle.label(), None);
        assert_eq!(AgentState::Working.label(), Some("Working"));
    }
}
