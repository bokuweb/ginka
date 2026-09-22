//! View models for the shell.
//!
//! A row is built from one `WorkspaceSummary` — the daemon's answer, already
//! carrying the worktree, its git status and whatever session is running in
//! it. Nothing here reads a database or shells out to git: by the time a
//! summary arrives, that work is done.

use crate::assets::icon;
use ginka_protocol::ids::slugify;
use ginka_protocol::model::{
    AgentStatus, BranchStatus, Project, ProjectKind, SessionState, WorkspaceSummary,
};
use ginka_protocol::{ProjectName, SessionId, WorkspaceId};
use gpui::SharedString;
use gpui_component::Icon;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::PathBuf;

/// The sidebar's rows, under the project they belong to.
///
/// The project is a heading rather than a line on every row: repeated once per
/// workspace it is noise, and a reader scanning for "which of my projects is
/// this" wants one place to look.
#[derive(Debug, Clone)]
pub struct ProjectGroup {
    /// What the daemon calls it, which is what every request naming a project
    /// carries.
    pub name: ProjectName,
    /// What the heading says.
    pub project: SharedString,
    /// The rows, each with the index it had in the flat list — which is what
    /// selection is addressed by.
    pub rows: Vec<(usize, SessionRow)>,
}

impl ProjectGroup {
    /// The most urgent thing happening in this project, for ordering.
    fn rank(&self) -> u8 {
        self.rows
            .iter()
            .map(|(_, row)| row.attention_rank())
            .min()
            .unwrap_or(u8::MAX)
    }
}

/// A registered project, as the sidebar's headings and the composer's project
/// chip show it.
///
/// The daemon's [`Project`] carries more than a row needs — a sort order, an
/// origin probe — and a view that read the wire type would be re-deciding what
/// to show every time one grew a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    /// What every request naming this project carries.
    pub name: ProjectName,
    /// What the row says. This is the optional name chosen in the add-project
    /// dialog, falling back to the stable key for older registrations.
    pub label: SharedString,
    /// Where it is *on the daemon's host* (`docs/roadmap.md` §4.1), shown
    /// under the label so two checkouts of one repository can be told apart.
    pub path: SharedString,
    /// What a new worktree here would be cut from. Empty for a plain folder,
    /// which has no branches at all.
    pub default_branch: SharedString,
    pub kind: ProjectKind,
}

impl ProjectRow {
    /// Build a row from the daemon's answer.
    pub fn from_project(project: &Project) -> Self {
        Self {
            name: project.name.clone(),
            label: project
                .label
                .clone()
                .unwrap_or_else(|| project.name.0.clone())
                .into(),
            path: project.path.to_string_lossy().to_string().into(),
            default_branch: project.default_branch.clone().into(),
            kind: project.kind,
        }
    }
}

/// A complete add-project dialog submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDraft {
    /// Human-readable name shown in the project rail.
    pub label: String,
    /// Source folder registered with the daemon.
    pub path: PathBuf,
}

/// Why the add-project dialog cannot be submitted yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectDraftError {
    /// The display name is empty after trimming whitespace.
    MissingName,
    /// No source folder has been chosen.
    MissingSource,
}

/// Validate and normalize values from the add-project dialog.
///
/// The name is checked before the source so Return from the focused name field
/// reports the field the user is currently editing.
pub fn validate_project_draft(
    name: &str,
    path: Option<PathBuf>,
) -> Result<ProjectDraft, ProjectDraftError> {
    let label = name.trim();
    if label.is_empty() {
        return Err(ProjectDraftError::MissingName);
    }
    let path = path.ok_or(ProjectDraftError::MissingSource)?;
    Ok(ProjectDraft {
        label: label.to_string(),
        path,
    })
}

/// The sidebar's tree: every registered project, with the workspaces in it.
///
/// Built from the projects rather than from the rows, so a project with no
/// workspace yet is still a heading the reader can select and start a chat
/// under — a list assembled from workspaces alone can only show the projects
/// that already have one, which is the wrong way round for a window whose
/// front door is "pick a project and say something".
///
/// A row whose project is not in `projects` still gets a group of its own: the
/// two lists come from two requests, and a workspace with nowhere to hang is
/// better shown under a heading of its own name than dropped.
pub fn tree(projects: &[ProjectRow], rows: &[SessionRow]) -> Vec<ProjectGroup> {
    let mut groups: Vec<ProjectGroup> = projects
        .iter()
        .map(|project| ProjectGroup {
            name: project.name.clone(),
            project: project.label.clone(),
            rows: Vec::new(),
        })
        .collect();

    for (index, row) in rows.iter().enumerate() {
        match groups.iter_mut().find(|group| group.name.0 == row.origin) {
            Some(group) => group.rows.push((index, row.clone())),
            None => groups.push(ProjectGroup {
                name: ProjectName(row.origin.to_string()),
                project: row.origin.clone(),
                rows: vec![(index, row.clone())],
            }),
        }
    }

    // Stable, so projects the daemon ordered keep that order among equals.
    groups.sort_by_key(ProjectGroup::rank);
    groups
}

/// The workspace a new chat in `project` starts in.
///
/// The worktree on the project's default branch when there is one: that is the
/// checkout a person means by "this project". Failing that, the first
/// workspace listed. `None` when the project has none at all, and the caller
/// says so rather than inventing a worktree behind a prompt — cutting a branch
/// is not what "new chat" said.
pub fn workspace_for_new_chat(rows: &[SessionRow], project: &ProjectRow) -> Option<WorkspaceId> {
    let mut in_project = rows.iter().filter(|row| row.origin == project.name.0);
    let first = in_project.next()?;
    if project.default_branch.is_empty() {
        return Some(first.workspace.clone());
    }
    let on_default = std::iter::once(first)
        .chain(in_project)
        .find(|row| row.branch == project.default_branch);
    Some(on_default.unwrap_or(first).workspace.clone())
}

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

    /// The driver id this agent is started with.
    ///
    /// The inverse of [`Agent::from_id`]: the sidebar shows a glyph, and
    /// starting a session from that row has to name the driver again.
    pub fn driver_id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
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

    /// What the row says it is doing, in the user's language.
    ///
    /// `None` for an idle row: it shows the age of its last commit instead,
    /// and a word there would be noise on every quiet workspace.
    pub fn label(self) -> Option<SharedString> {
        match self {
            Self::Working => Some(rust_i18n::t!("status.working").to_string().into()),
            Self::NeedsAttention => Some(rust_i18n::t!("status.attention").to_string().into()),
            Self::Idle => None,
        }
    }
}

/// One row in the session list.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// What the row is about. Every request the centre column makes — start an
    /// agent, read a transcript, list checkpoints — is addressed by this.
    pub workspace: WorkspaceId,
    /// The session running in it, when there is one.
    pub session: Option<SessionId>,
    pub title: SharedString,
    pub agent: Agent,
    /// Model used by the latest session in this workspace.
    pub model: Option<String>,
    /// Reasoning level used by later turns of that session.
    pub reasoning_effort: Option<String>,
    /// Service tier used by later turns of that session.
    pub service_tier: Option<String>,
    /// The login the session runs on, when there is a session. A follow-up
    /// stays on it (`docs/accounts.md` §5).
    pub account: Option<ginka_protocol::AccountId>,
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
    /// Whether semantic search is ready for agents started in this worktree.
    pub indexed: bool,
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
            workspace: summary.id(),
            session: session.map(|session| session.id.clone()),
            title: title_for(&summary.worktree.name).into(),
            account: session.map(|session| session.account.clone()),
            model: session.and_then(|session| session.model.clone()),
            reasoning_effort: session.and_then(|session| session.reasoning_effort.clone()),
            service_tier: session.and_then(|session| session.service_tier.clone()),
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
            archived: summary.worktree.archived,
            indexed: summary.indexed,
        }
    }

    /// Whether the branch is worth a line of its own.
    ///
    /// A workspace's name comes from the branch it was cut on, so most rows
    /// would spend a line repeating their own title. The exception is the one
    /// that matters: an agent that checked out something else inside the
    /// worktree, which is exactly when the reader needs to see it.
    pub fn branch_worth_showing(&self) -> bool {
        !self.branch.is_empty() && slugify(&self.branch) != slugify(&self.title)
    }

    /// Which agent the composer would start here.
    ///
    /// What the user picked wins — that is what picking is for, including
    /// picking one this build cannot probe or that says it is signed out.
    /// Failing that, a workspace already holding a session continues with the
    /// agent running it, since switching mid-conversation would abandon the
    /// transcript the vendor is holding. A fresh workspace gets the first agent
    /// that is actually usable: starting one that is signed out only produces a
    /// failed session and a puzzled user.
    pub fn agent_to_start(&self, agents: &[AgentStatus], chosen: Option<&str>) -> String {
        if let Some(chosen) = chosen {
            return chosen.to_string();
        }
        if self.session.is_some() {
            return self.agent.driver_id().to_string();
        }
        agents
            .iter()
            .find(|agent| agent.is_ready())
            .map(|agent| agent.id.clone())
            .unwrap_or_else(|| self.agent.driver_id().to_string())
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
            workspace: WorkspaceId(format!("sample/{}", title.to_lowercase().replace(' ', "-"))),
            session: None,
            title: title.into(),
            agent,
            account: None,
            model: None,
            reasoning_effort: None,
            service_tier: None,
            status: BranchStatus::default(),
            path: PathBuf::new(),
            origin: origin.into(),
            branch: branch.into(),
            state,
            age: age.into(),
            archived,
            indexed: false,
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

/// Whether a sidebar row fuzzy-matches visible conversation metadata.
///
/// Search covers the title, model, provider, project and branch independently;
/// joining them first would let a query cross field boundaries that are not
/// visible next to each other.
pub fn session_matches(row: &SessionRow, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let fields = [
        row.title.as_ref(),
        row.model.as_deref().unwrap_or_default(),
        row.agent.label(),
        row.origin.as_ref(),
        row.branch.as_ref(),
    ];
    fields.into_iter().any(|field| {
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buffer = Vec::new();
        let haystack = nucleo_matcher::Utf32Str::new(field, &mut buffer);
        pattern.score(haystack, &mut matcher).is_some()
    })
}

/// The first nine visible active workspaces addressed by number shortcuts.
///
/// The order is exactly the selected project's session list: attention first,
/// then the daemon's stable order among equal states. Archived rows and rows
/// hidden by the current search do not acquire an invisible shortcut.
pub fn session_shortcuts(
    rows: &[SessionRow],
    project: &ProjectName,
    query: &str,
) -> Vec<WorkspaceId> {
    let mut visible = rows
        .iter()
        .filter(|row| !row.archived)
        .filter(|row| row.origin.as_ref() == project.0)
        .filter(|row| session_matches(row, query))
        .collect::<Vec<_>>();
    visible.sort_by_key(|row| row.attention_rank());
    visible
        .into_iter()
        .take(9)
        .map(|row| row.workspace.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_shortcuts_follow_the_visible_projects_attention_order() {
        let mut rows = SessionRow::samples();
        rows.truncate(5);
        for row in &mut rows {
            row.origin = "comet".into();
            row.archived = false;
        }
        rows[0].state = AgentState::Idle;
        rows[1].state = AgentState::Working;
        rows[2].state = AgentState::NeedsAttention;
        rows[3].origin = "nebula".into();
        rows[4].archived = true;

        let shortcuts = session_shortcuts(&rows, &ProjectName("comet".into()), "");
        assert_eq!(
            shortcuts,
            vec![
                rows[1].workspace.clone(),
                rows[2].workspace.clone(),
                rows[0].workspace.clone(),
            ]
        );
    }

    #[test]
    fn numbered_shortcuts_respect_search_and_stop_at_nine() {
        let sample = SessionRow::samples().remove(0);
        let rows = (0..12)
            .map(|index| {
                let mut row = sample.clone();
                row.workspace = WorkspaceId(format!("comet/task-{index}"));
                row.title = format!("Task {index}").into();
                row.origin = "comet".into();
                row
            })
            .collect::<Vec<_>>();

        assert_eq!(
            session_shortcuts(&rows, &ProjectName("comet".into()), "").len(),
            9
        );
        assert_eq!(
            session_shortcuts(&rows, &ProjectName("comet".into()), "task 11"),
            vec![WorkspaceId("comet/task-11".into())]
        );
    }

    #[test]
    fn a_project_draft_requires_a_name_and_source_folder() {
        assert_eq!(
            validate_project_draft("   ", Some(PathBuf::from("/tmp/comet"))),
            Err(ProjectDraftError::MissingName)
        );
        assert_eq!(
            validate_project_draft("Comet", None),
            Err(ProjectDraftError::MissingSource)
        );
    }

    #[test]
    fn a_project_draft_trims_the_display_name_before_submission() {
        assert_eq!(
            validate_project_draft("  Comet  ", Some(PathBuf::from("/tmp/comet"))),
            Ok(ProjectDraft {
                label: "Comet".into(),
                path: PathBuf::from("/tmp/comet"),
            })
        );
    }

    #[test]
    fn a_project_row_prefers_the_dialog_s_display_name_to_its_stable_key() {
        let mut project = Project {
            name: ProjectName("comet-checkout".into()),
            path: PathBuf::from("/tmp/comet-checkout"),
            default_branch: "main".into(),
            label: Some("Comet".into()),
            sort_order: 0,
            kind: ProjectKind::Git,
            has_origin: Some(true),
        };
        assert_eq!(ProjectRow::from_project(&project).label, "Comet");

        project.label = None;
        assert_eq!(ProjectRow::from_project(&project).label, "comet-checkout");
    }

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
    fn sidebar_search_matches_title_model_provider_and_branch() {
        let mut row = SessionRow::samples().remove(0);
        row.model = Some("gpt-5.6-sol".into());
        row.agent = Agent::Codex;
        row.branch = "feature/usage-meter".into();

        assert!(session_matches(&row, "story"));
        assert!(session_matches(&row, "gpt56"), "fuzzy model match");
        assert!(session_matches(&row, "CODEX"));
        assert!(session_matches(&row, "usgmtr"), "fuzzy branch match");
        assert!(session_matches(&row, "  "));
        assert!(!session_matches(&row, "sonnet"));
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
        assert!(AgentState::Working.label().is_some());
    }

    #[test]
    fn status_words_are_translated() {
        // The locale is one global for the process, so this test puts it back.
        // No other test here asserts an English string while it is switched.
        ginka_core::i18n::apply("ja");
        assert_eq!(
            AgentState::Working.label().as_deref(),
            Some("実行中"),
            "a Japanese desktop should not be shown English status words"
        );
        ginka_core::i18n::apply("en");
        assert_eq!(AgentState::Working.label().as_deref(), Some("Working"));
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
                archived: false,
            },
            status: BranchStatus::default(),
            session,
            last_commit_at: Some(900_000),
            indexed: false,
        }
    }

    fn session(agent: &str, state: SessionState) -> ginka_protocol::model::Session {
        ginka_protocol::model::Session {
            id: ginka_protocol::SessionId("s".into()),
            workspace: ginka_protocol::WorkspaceId("comet/remove-r2-file-uploads".into()),
            agent: agent.into(),
            account: ginka_protocol::AccountId(agent.into()),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            state,
            title: None,
            summary: None,
            vendor_session_id: None,
            access_mode: Default::default(),
            origin: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn a_workspace_with_no_session_is_still_a_row() {
        // It is how the user starts one; hiding it would hide the way in.
        let row = SessionRow::from_summary(&summary(None), 1_000_000);
        assert_eq!(row.workspace.0, "comet/remove-r2-file-uploads");
        assert_eq!(row.session, None);
        assert_eq!(row.title, "Remove R2 File Uploads");
        assert_eq!(row.origin, "comet");
        assert_eq!(row.branch, "remove/r2-file-uploads");
        assert_eq!(row.state, AgentState::Idle);
        assert_eq!(row.age, "1d");
    }

    #[test]
    fn archive_state_reaches_the_sidebar_row() {
        let mut summary = summary(None);
        summary.worktree.archived = true;
        let row = SessionRow::from_summary(&summary, 1_000_000);
        assert!(row.archived);
    }

    #[test]
    fn semantic_index_state_reaches_the_composer_row() {
        let mut summary = summary(None);
        summary.indexed = true;
        assert!(SessionRow::from_summary(&summary, 1_000_000).indexed);
    }

    #[test]
    fn provider_options_reach_the_composer_row() {
        let mut active = session("codex", SessionState::Idle);
        active.model = Some("gpt-next".into());
        active.reasoning_effort = Some("high".into());
        active.service_tier = Some("priority".into());
        let row = SessionRow::from_summary(&summary(Some(active)), 1_000_000);
        assert_eq!(row.model.as_deref(), Some("gpt-next"));
        assert_eq!(row.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(row.service_tier.as_deref(), Some("priority"));
    }

    #[test]
    fn a_row_takes_its_glyph_and_status_from_the_session_running_in_it() {
        let row = SessionRow::from_summary(
            &summary(Some(session("codex", SessionState::Running))),
            1_000_000,
        );
        assert_eq!(row.agent, Agent::Codex);
        assert_eq!(row.state, AgentState::Working);
        assert_eq!(
            row.session,
            Some(ginka_protocol::SessionId("s".into())),
            "the centre column addresses the transcript by this"
        );
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

    fn status(id: &str, installed: bool, authenticated: Option<bool>) -> AgentStatus {
        AgentStatus {
            id: id.into(),
            display_name: id.into(),
            program: id.into(),
            installed,
            version: None,
            authenticated,
            detail: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn a_fresh_workspace_starts_the_agent_that_is_actually_usable() {
        // Starting one that is signed out produces a failed session and a
        // puzzled user; the one next to it would have worked.
        let row = SessionRow::from_summary(&summary(None), 0);
        let agents = vec![
            status("claude", true, Some(false)),
            status("codex", true, Some(true)),
        ];
        assert_eq!(row.agent_to_start(&agents, None), "codex");
    }

    #[test]
    fn what_the_user_picked_wins_over_what_would_have_been_chosen() {
        // Including an agent that says it is signed out: the user may be
        // fixing that in another window, and a picker that refuses the pick is
        // not a picker.
        let row = SessionRow::from_summary(&summary(None), 0);
        let agents = vec![
            status("claude", true, Some(false)),
            status("codex", true, Some(true)),
        ];
        assert_eq!(row.agent_to_start(&agents, Some("claude")), "claude");
    }

    #[test]
    fn a_workspace_with_a_session_stays_with_the_agent_running_it() {
        // Switching mid-conversation would abandon the transcript the vendor
        // is holding.
        let row = SessionRow::from_summary(&summary(Some(session("codex", SessionState::Idle))), 0);
        let agents = vec![status("claude", true, Some(true))];
        assert_eq!(row.agent_to_start(&agents, None), "codex");
    }

    #[test]
    fn with_nothing_usable_the_default_is_still_offered() {
        // The failure then comes from the agent, with its own words, rather
        // than from a composer that refused to do anything.
        let row = SessionRow::from_summary(&summary(None), 0);
        assert_eq!(row.agent_to_start(&[], None), "claude");
        assert_eq!(
            row.agent_to_start(&[status("claude", false, None)], None),
            "claude"
        );
    }

    fn project_row(name: &str, default_branch: &str) -> ProjectRow {
        ProjectRow {
            name: ProjectName(name.into()),
            label: name.into(),
            path: format!("/tmp/{name}").into(),
            default_branch: default_branch.into(),
            kind: ProjectKind::Git,
        }
    }

    #[test]
    fn rows_are_grouped_under_their_project_in_the_order_they_came() {
        let rows = SessionRow::samples();
        let groups = tree(&[project_row("ginka @ personal-metal", "main")], &rows);
        assert_eq!(groups.len(), 1, "the samples are all one project");
        let indices: Vec<usize> = groups[0].rows.iter().map(|(index, _)| *index).collect();
        assert_eq!(indices, (0..rows.len()).collect::<Vec<_>>());
    }

    #[test]
    fn a_project_with_no_workspace_yet_is_still_a_heading() {
        // It is where "new chat in this project" is chosen from, and a tree
        // built from workspaces alone could not show it at all.
        let groups = tree(&[project_row("empty", "main")], &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name.0, "empty");
        assert!(groups[0].rows.is_empty());
    }

    #[test]
    fn a_workspace_whose_project_is_not_listed_still_appears() {
        // Two requests answer at different moments; dropping the row would
        // hide a running agent because a list arrived late.
        let mut row = SessionRow::from_summary(&summary(None), 0);
        row.origin = "unlisted".into();
        let groups = tree(&[], &[row]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name.0, "unlisted");
    }

    #[test]
    fn a_project_with_an_agent_working_in_it_rises() {
        let mut quiet = SessionRow::from_summary(&summary(None), 0);
        quiet.origin = "quiet".into();
        let mut busy =
            SessionRow::from_summary(&summary(Some(session("claude", SessionState::Running))), 0);
        busy.origin = "busy".into();

        let groups = tree(
            &[project_row("quiet", "main"), project_row("busy", "main")],
            &[quiet, busy],
        );
        assert_eq!(groups[0].project, "busy");
        assert_eq!(
            groups[0].rows[0].0, 1,
            "the row keeps the index selection is addressed by"
        );
    }

    #[test]
    fn a_new_chat_lands_on_the_project_s_default_branch() {
        // "This project" means the checkout a person would open in an editor,
        // not whichever worktree happens to be listed first.
        let mut feature = SessionRow::from_summary(&summary(None), 0);
        feature.origin = "comet".into();
        feature.branch = "feature/one".into();
        let mut main = SessionRow::from_summary(&summary(None), 0);
        main.origin = "comet".into();
        main.branch = "main".into();
        main.workspace = WorkspaceId("comet/main".into());

        let chosen = workspace_for_new_chat(&[feature, main], &project_row("comet", "main"));
        assert_eq!(chosen, Some(WorkspaceId("comet/main".into())));
    }

    #[test]
    fn a_project_without_that_branch_checked_out_still_takes_a_chat() {
        let mut feature = SessionRow::from_summary(&summary(None), 0);
        feature.origin = "comet".into();
        feature.branch = "feature/one".into();
        let id = feature.workspace.clone();

        assert_eq!(
            workspace_for_new_chat(&[feature], &project_row("comet", "main")),
            Some(id),
            "the reader asked for a chat, not for a branch to be cut"
        );
    }

    #[test]
    fn a_plain_folder_takes_its_one_workspace() {
        // It has no default branch, so there is nothing to prefer.
        let mut row = SessionRow::from_summary(&summary(None), 0);
        row.origin = "notes".into();
        let id = row.workspace.clone();
        let folder = ProjectRow {
            kind: ProjectKind::Plain,
            ..project_row("notes", "")
        };
        assert_eq!(workspace_for_new_chat(&[row], &folder), Some(id));
    }

    #[test]
    fn a_project_with_nothing_in_it_has_nowhere_to_start_a_chat() {
        // The caller says so rather than cutting a branch behind a prompt.
        assert_eq!(
            workspace_for_new_chat(&[], &project_row("comet", "main")),
            None
        );
    }

    #[test]
    fn a_branch_that_only_repeats_the_title_is_not_shown() {
        // Most workspaces are named after the branch they were cut on, and a
        // line of every row spent repeating its own title is a line wasted.
        let mut row = SessionRow::from_summary(&summary(None), 0);
        assert_eq!(row.title, "Remove R2 File Uploads");
        row.branch = "remove-r2-file-uploads".into();
        assert!(!row.branch_worth_showing());

        // Until an agent checks out something else inside the worktree, which
        // is exactly when the reader needs to see it.
        row.branch = "fix/something-else".into();
        assert!(row.branch_worth_showing());
    }

    #[test]
    fn a_glyph_names_the_driver_it_came_from() {
        // The row is what the user starts an agent from, so the mapping has to
        // survive the round trip.
        for agent in [Agent::Claude, Agent::Codex, Agent::Gemini] {
            assert_eq!(Agent::from_id(agent.driver_id()), agent);
        }
    }
}
