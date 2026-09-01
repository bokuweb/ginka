//! View models for the shell.
//!
//! A row is built from one `WorkspaceSummary` — the daemon's answer, already
//! carrying the worktree, its git status and whatever session is running in
//! it. Nothing here reads a database or shells out to git: by the time a
//! summary arrives, that work is done.

use crate::assets::icon;
use ginka_protocol::model::{BranchStatus, SessionState, WorkspaceSummary};
use gpui::SharedString;
use gpui_component::Icon;
use std::path::PathBuf;

/// Turn a worktree's slug into something readable.
///
/// `remove-r2-file-uploads` reads as a machine key; `Remove R2 File Uploads`
/// reads as a task. Until sessions carry their own titles (M2), the workspace
/// name is the only thing there is to show.
fn title_for(name: &str) -> String {
    let mut title = String::with_capacity(name.len());
    for (index, word) in name.split('-').filter(|word| !word.is_empty()).enumerate() {
        if index > 0 {
            title.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            title.extend(first.to_uppercase());
            title.push_str(chars.as_str());
        }
    }
    if title.is_empty() {
        name.to_string()
    } else {
        title
    }
}

/// Compress an age into the two or three characters the sidebar has room for.
///
/// Not git's `%cr`: that is localized and its wording has changed between
/// versions, so a row would read "vor 4 Stunden" on one machine and "4 hours
/// ago" on another. A clock skew that puts the commit in the future reads as
/// "now" rather than a negative number.
pub fn relative_age(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    match seconds {
        ..60 => "now".to_string(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..2_592_000 => format!("{}d", seconds / 86_400),
        _ => format!("{}mo", seconds / 2_592_000),
    }
}

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
    /// The agent a driver id names.
    ///
    /// An id this build has no glyph for reads as Claude rather than as
    /// nothing: a row with no mark is harder to scan than a row with the wrong
    /// one, and the agent's name is on the row's detail anyway.
    pub fn from_id(id: &str) -> Self {
        match id {
            "codex" => Self::Codex,
            "gemini" => Self::Gemini,
            _ => Self::Claude,
        }
    }

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
    /// How a session's state reads in the sidebar.
    ///
    /// A failed session needs the user as much as a question does — it is the
    /// row they have to go and look at — so both sort into `NeedsAttention`.
    pub fn from_session(state: SessionState) -> Self {
        match state {
            SessionState::Starting | SessionState::Running => Self::Working,
            SessionState::AwaitingInput | SessionState::Failed => Self::NeedsAttention,
            SessionState::Idle | SessionState::Finished | SessionState::Cancelled => Self::Idle,
        }
    }

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
    /// How the worktree stands against its upstream, drawn beside the branch.
    pub status: BranchStatus,
    /// The worktree on disk, shown by the terminal and the context bar.
    pub path: PathBuf,
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
    /// Build a sidebar row from the daemon's summary of a workspace.
    ///
    /// A workspace with no session yet is still a row — that is how the user
    /// starts one — and it shows the age of its last commit instead of a
    /// status word.
    pub fn from_summary(summary: &WorkspaceSummary, now: i64) -> Self {
        let session = summary.session.as_ref();
        Self {
            title: title_for(&summary.worktree.name).into(),
            agent: session
                .map(|session| Agent::from_id(&session.agent))
                .unwrap_or(Agent::Claude),
            status: summary.status,
            path: summary.worktree.path.clone(),
            origin: summary.worktree.project.0.clone().into(),
            branch: summary.worktree.branch.clone().into(),
            state: session
                .map(|session| AgentState::from_session(session.state))
                .unwrap_or(AgentState::Idle),
            // A running agent shows what it is doing; the timestamp is for the
            // rows that have nothing to say.
            age: summary
                .last_commit_at
                .map(|then| relative_age(now, then))
                .unwrap_or_default()
                .into(),
            archived: false,
        }
    }

    /// A short summary of the worktree's divergence, or `None` when there is
    /// nothing to say.
    ///
    /// A branch with no upstream reports nothing rather than "0↑ 0↓", which
    /// would claim it is in sync with something that does not exist.
    pub fn divergence(&self) -> Option<String> {
        if self.status.untracked_branch {
            return None;
        }
        match (self.status.ahead, self.status.behind) {
            (0, 0) => None,
            (ahead, 0) => Some(format!("{ahead}↑")),
            (0, behind) => Some(format!("{behind}↓")),
            (ahead, behind) => Some(format!("{ahead}↑ {behind}↓")),
        }
    }

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
            status: BranchStatus::default(),
            path: PathBuf::new(),
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
    fn ages_compress_to_what_the_row_can_hold() {
        assert_eq!(relative_age(1_000_000, 1_000_000), "now");
        assert_eq!(relative_age(1_000_000, 999_970), "now");
        assert_eq!(relative_age(1_000_000, 999_400), "10m");
        assert_eq!(relative_age(1_000_000, 985_600), "4h");
        assert_eq!(relative_age(1_000_000, 900_000), "1d");
        assert_eq!(relative_age(10_000_000, 1_000_000), "3mo");
    }

    #[test]
    fn a_commit_in_the_future_reads_as_now_rather_than_a_negative_age() {
        // Clock skew between machines sharing a repository is ordinary.
        assert_eq!(relative_age(1_000, 9_999), "now");
    }

    #[test]
    fn divergence_stays_silent_without_an_upstream() {
        let mut row = SessionRow::samples().remove(0);
        row.status = BranchStatus {
            untracked_branch: true,
            ahead: 0,
            behind: 0,
            ..Default::default()
        };
        assert_eq!(row.divergence(), None);

        row.status = BranchStatus {
            untracked_branch: false,
            ahead: 2,
            behind: 0,
            ..Default::default()
        };
        assert_eq!(row.divergence().as_deref(), Some("2↑"));

        row.status = BranchStatus {
            untracked_branch: false,
            ahead: 2,
            behind: 3,
            ..Default::default()
        };
        assert_eq!(row.divergence().as_deref(), Some("2↑ 3↓"));

        row.status = BranchStatus {
            untracked_branch: false,
            ..Default::default()
        };
        assert_eq!(row.divergence(), None, "in sync says nothing");
    }

    #[test]
    fn slugs_become_readable_titles() {
        assert_eq!(
            title_for("remove-r2-file-uploads"),
            "Remove R2 File Uploads"
        );
        assert_eq!(title_for("main"), "Main");
        // Degenerate input still produces something to render rather than an
        // empty row.
        assert_eq!(title_for(""), "");
        assert_eq!(title_for("--"), "--");
    }

    #[test]
    fn idle_rows_show_a_timestamp_rather_than_a_status_word() {
        assert_eq!(AgentState::Idle.label(), None);
        assert_eq!(AgentState::Working.label(), Some("Working"));
    }

    fn summary(session: Option<ginka_protocol::model::Session>) -> WorkspaceSummary {
        WorkspaceSummary {
            worktree: ginka_protocol::model::Worktree {
                project: ginka_protocol::ProjectName("comet".into()),
                name: "remove-r2-file-uploads".into(),
                branch: "remove/r2-file-uploads".into(),
                path: PathBuf::from("/tmp/wt"),
                head: None,
                pinned: false,
            },
            status: BranchStatus::default(),
            session,
            last_commit_at: Some(900_000),
        }
    }

    fn session(agent: &str, state: SessionState) -> ginka_protocol::model::Session {
        ginka_protocol::model::Session {
            id: ginka_protocol::SessionId("s".into()),
            workspace: ginka_protocol::WorkspaceId("comet/remove-r2-file-uploads".into()),
            agent: agent.into(),
            model: None,
            state,
            summary: None,
            vendor_session_id: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn a_workspace_with_no_session_is_still_a_row() {
        // It is how the user starts one; hiding it would hide the way in.
        let row = SessionRow::from_summary(&summary(None), 1_000_000);
        assert_eq!(row.title, "Remove R2 File Uploads");
        assert_eq!(row.origin, "comet");
        assert_eq!(row.branch, "remove/r2-file-uploads");
        assert_eq!(row.state, AgentState::Idle);
        assert_eq!(row.age, "1d");
    }

    #[test]
    fn a_row_takes_its_glyph_and_status_from_the_session_running_in_it() {
        let row = SessionRow::from_summary(
            &summary(Some(session("codex", SessionState::Running))),
            1_000_000,
        );
        assert_eq!(row.agent, Agent::Codex);
        assert_eq!(row.state, AgentState::Working);
    }

    #[test]
    fn a_failed_session_pulls_its_row_up_beside_the_ones_asking_a_question() {
        let asked = SessionRow::from_summary(
            &summary(Some(session("claude", SessionState::AwaitingInput))),
            0,
        );
        let failed =
            SessionRow::from_summary(&summary(Some(session("claude", SessionState::Failed))), 0);
        assert_eq!(asked.state, AgentState::NeedsAttention);
        assert_eq!(
            failed.attention_rank(),
            asked.attention_rank(),
            "a failure is something to go and look at, like a question"
        );
    }

    #[test]
    fn an_agent_this_build_has_no_glyph_for_still_marks_its_row() {
        assert_eq!(Agent::from_id("amp"), Agent::Claude);
        assert_eq!(Agent::from_id("codex"), Agent::Codex);
    }
}
