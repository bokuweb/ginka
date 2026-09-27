//! What a view draws.
//!
//! One type per thing on screen, shaped for drawing rather than for the wire:
//! the wire's shapes stop in `wire.rs`, and a view that read them directly
//! would be re-deciding what to show every time GitHub grew a field.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// `owner/name`: the key every repository-scoped thing hangs off.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoId {
    /// The user or organisation.
    pub owner: String,
    /// The repository.
    pub name: String,
}

impl RepoId {
    /// Build one from its two halves.
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// Parse `owner/name`. `None` when there is not exactly one slash with
    /// something either side of it.
    pub fn parse(full_name: &str) -> Option<Self> {
        let (owner, name) = full_name.split_once('/')?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self::new(owner, name))
    }

    /// Parse the repository out of an API URL such as
    /// `https://api.github.com/repos/owner/name/pulls/12`.
    pub fn from_api_url(url: &str) -> Option<Self> {
        let rest = url.split("/repos/").nth(1)?;
        let mut parts = rest.split('/');
        let owner = parts.next()?;
        let name = parts.next()?;
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self::new(owner, name))
    }

    /// Where the repository lives on the web.
    pub fn html_url(&self) -> String {
        format!("https://github.com/{self}")
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// A repository the viewer can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// Its key.
    pub id: RepoId,
    /// The one-line description, when there is one.
    pub description: Option<String>,
    /// Private repositories are marked in the sidebar.
    pub private: bool,
    /// What a pull request targets unless told otherwise.
    pub default_branch: String,
    /// Stargazers.
    pub stars: u64,
    /// Open issues and pulls together, as GitHub counts them.
    pub open_issues: u64,
    /// The last push, which is what the sidebar sorts by.
    pub pushed_at: Option<DateTime<Utc>>,
    /// Where it lives on the web.
    pub html_url: String,
}

/// A GitHub account, as it appears on an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// The login, which is what is shown; GitHub does not send display names
    /// on items.
    pub login: String,
    /// The avatar, for when avatars are drawn (M5).
    pub avatar_url: String,
}

/// Who the token is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewer {
    /// The login, used in the sidebar and in search queries (`author:login`).
    pub login: String,
    /// The display name, when set.
    pub name: Option<String>,
    /// The avatar.
    pub avatar_url: String,
}

/// A label on an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    /// The text.
    pub name: String,
    /// Six hex digits without a `#`, as GitHub sends it.
    pub color: String,
    /// What the label is for, when the repository said.
    #[serde(default)]
    pub description: Option<String>,
}

/// What kind of thing a notification is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubjectKind {
    /// A pull request.
    PullRequest,
    /// An issue.
    Issue,
    /// A release.
    Release,
    /// A discussion.
    Discussion,
    /// A commit.
    Commit,
    /// A check suite.
    CheckSuite,
    /// Something this app does not draw specially; the wire's own name.
    Other(String),
}

impl SubjectKind {
    /// Map GitHub's `subject.type`.
    pub fn parse(kind: &str) -> Self {
        match kind {
            "PullRequest" => Self::PullRequest,
            "Issue" => Self::Issue,
            "Release" => Self::Release,
            "Discussion" => Self::Discussion,
            "Commit" => Self::Commit,
            "CheckSuite" => Self::CheckSuite,
            other => Self::Other(other.to_string()),
        }
    }
}

/// One inbox row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// GitHub's thread id, which is what marking it read will take.
    pub id: String,
    /// Whether it is unread.
    pub unread: bool,
    /// Why it is in the inbox: `review_requested`, `mention`, `subscribed`…
    pub reason: String,
    /// When the thread last moved.
    pub updated_at: DateTime<Utc>,
    /// The repository it belongs to.
    pub repo: RepoId,
    /// The subject's title.
    pub title: String,
    /// What the subject is.
    pub kind: SubjectKind,
    /// The pull or issue number, when the subject has one.
    pub number: Option<u64>,
}

impl Notification {
    /// Where the subject lives on the web, when this app can say.
    ///
    /// GitHub only sends the API URL of the subject; the web URL is derived,
    /// and only for the kinds whose web path is known.
    pub fn html_url(&self) -> Option<String> {
        let number = self.number?;
        let segment = match self.kind {
            SubjectKind::PullRequest => "pull",
            SubjectKind::Issue => "issues",
            SubjectKind::Discussion => "discussions",
            _ => return None,
        };
        Some(format!("{}/{segment}/{number}", self.repo.html_url()))
    }
}

/// Whether an item is a pull or an issue, and what only a pull can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// An issue.
    Issue,
    /// A pull request.
    Pull {
        /// Marked as a draft.
        draft: bool,
        /// Merged. The wire never says "merged": a merged pull is `closed`
        /// with `merged_at` set, and this is where that is decided.
        merged: bool,
    },
}

/// Open or closed, as the wire says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Open.
    Open,
    /// Closed, merged pulls included.
    Closed,
}

/// The state a row draws, which is the wire's status refined by the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    /// An open issue or a ready pull.
    Open,
    /// A draft pull.
    Draft,
    /// A merged pull.
    Merged,
    /// A closed issue or an unmerged closed pull.
    Closed,
}

/// A pull request or an issue, as a list row and a detail header draw it.
///
/// One type for both because every list and every header draws them the same
/// way; the state glyph is the one difference, and [`Kind`] carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// The repository.
    pub repo: RepoId,
    /// The number, unique within the repository across pulls and issues.
    pub number: u64,
    /// GitHub's global id, which is what GraphQL — and so Projects — takes.
    #[serde(default)]
    pub node_id: String,
    /// The title.
    pub title: String,
    /// Pull or issue.
    pub kind: Kind,
    /// Open or closed.
    pub status: Status,
    /// Who opened it.
    pub author: User,
    /// When it was opened.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
    /// How many comments it has, when the endpoint said (pull lists do not).
    pub comments: Option<u64>,
    /// Its labels.
    pub labels: Vec<Label>,
    /// Who it is assigned to.
    pub assignees: Vec<User>,
    /// Whose review is requested. Empty for an issue.
    pub requested_reviewers: Vec<User>,
    /// Where it lives on the web.
    pub html_url: String,
    /// The description, as markdown. Empty when none was written.
    pub body: String,
}

impl Item {
    /// The state a view draws.
    pub fn state(&self) -> State {
        match (self.kind, self.status) {
            (Kind::Pull { merged: true, .. }, _) => State::Merged,
            (Kind::Pull { draft: true, .. }, Status::Open) => State::Draft,
            (_, Status::Open) => State::Open,
            (_, Status::Closed) => State::Closed,
        }
    }

    /// Whether this is a pull request.
    pub fn is_pull(&self) -> bool {
        matches!(self.kind, Kind::Pull { .. })
    }
}

/// A pull request with what only a pull has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pull {
    /// Everything it shares with an issue.
    pub item: Item,
    /// The branch being merged.
    pub head: String,
    /// The branch it merges into.
    pub base: String,
    /// Lines added.
    pub additions: u64,
    /// Lines removed.
    pub deletions: u64,
    /// Files touched.
    pub changed_files: u64,
    /// Whether GitHub thinks it can be merged. `None` while GitHub is still
    /// computing it, which is the usual answer right after a push.
    pub mergeable: Option<bool>,
    /// The commit at the head, which is what checks are keyed by.
    #[serde(default)]
    pub head_sha: String,
}

/// What one check or status came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckState {
    /// Passed.
    Success,
    /// Failed, errored, timed out, or wants action.
    Failure,
    /// Queued or running.
    Pending,
    /// Neither passed nor failed: skipped, or neutral.
    Neutral,
}

/// One check run, or one commit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    /// GitHub's id for the run — for an Actions job, the job's id, which is
    /// what its log is fetched by. Zero for a commit status.
    #[serde(default)]
    pub id: u64,
    /// Whether it is a GitHub Actions job, whose log the API can read.
    #[serde(default)]
    pub actions: bool,
    /// The check's name, or the status's context.
    pub name: String,
    /// What it came to.
    pub state: CheckState,
    /// Where its details are, when it said.
    pub html_url: Option<String>,
}

/// Every check and status on a commit, together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Checks {
    /// The runs, in GitHub's order.
    pub runs: Vec<CheckRun>,
}

impl Checks {
    /// How many are in each state: `(passed, failed, pending)`. Neutral runs
    /// count as passed, the way GitHub's own summary counts them.
    pub fn tally(&self) -> (usize, usize, usize) {
        let mut tally = (0, 0, 0);
        for run in &self.runs {
            match run.state {
                CheckState::Success | CheckState::Neutral => tally.0 += 1,
                CheckState::Failure => tally.1 += 1,
                CheckState::Pending => tally.2 += 1,
            }
        }
        tally
    }

    /// The one state the whole set is in: a failure outranks a pending run,
    /// which outranks success, and no runs at all is neutral.
    pub fn overall(&self) -> CheckState {
        let (_, failed, pending) = self.tally();
        if self.runs.is_empty() {
            CheckState::Neutral
        } else if failed > 0 {
            CheckState::Failure
        } else if pending > 0 {
            CheckState::Pending
        } else {
            CheckState::Success
        }
    }
}

/// What a review says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewEvent {
    /// Approve.
    Approve,
    /// Ask for changes.
    RequestChanges,
    /// Neither: a review that is only its comment.
    Comment,
}

impl ReviewEvent {
    /// The value GitHub's `event` field takes.
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }
}

/// How a pull is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MergeMethod {
    /// A merge commit.
    #[default]
    Merge,
    /// One squashed commit.
    Squash,
    /// Rebased onto the base.
    Rebase,
}

impl MergeMethod {
    /// All three, in the order the chips show them.
    pub const ALL: &'static [MergeMethod] =
        &[MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase];

    /// The value GitHub's `merge_method` field takes.
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

/// A GitHub Project (the current kind, "Projects v2").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// The project's global id, which adding an item takes.
    pub id: String,
    /// The user or organisation that owns it.
    pub owner: String,
    /// Its title.
    pub title: String,
    /// Its number within the owner.
    pub number: u64,
    /// Whether it is closed.
    pub closed: bool,
    /// Where the project lives on GitHub.
    pub html_url: String,
}

/// The kind of content a Projects v2 item holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectItemKind {
    /// A project-only draft issue.
    DraftIssue,
    /// A repository issue.
    Issue,
    /// A repository pull request.
    PullRequest,
    /// Content the token is not allowed to read any more.
    Redacted,
}

/// One card or row inside a GitHub Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectItem {
    /// The item's id within the project.
    pub id: String,
    /// Its visible title.
    pub title: String,
    /// What kind of content it points at.
    pub kind: ProjectItemKind,
    /// The repository, for issues and pull requests.
    pub repo: Option<RepoId>,
    /// The issue or pull request number.
    pub number: Option<u64>,
    /// GitHub's open/closed state, when the content has one.
    pub state: Option<String>,
    /// The selected value of the field named `Status`, when one is set.
    pub status: Option<String>,
    /// Values of custom fields used by saved board and roadmap views.
    pub fields: Vec<ProjectFieldValue>,
    /// Where the content lives on GitHub, when it has a page.
    pub html_url: Option<String>,
    /// Whether the project item is archived.
    pub archived: bool,
}

impl ProjectItem {
    /// The native item detail this project row can open.
    pub fn item_key(&self) -> Option<(RepoId, u64)> {
        Some((self.repo.clone()?, self.number?))
    }
}

/// The layout GitHub saved for a Project view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectViewLayout {
    /// Rows and fields.
    Table,
    /// Cards arranged into columns.
    Board,
    /// Items placed on a date axis.
    Roadmap,
}

/// A field that can configure or supply values to a Project view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectField {
    /// The field's global id.
    pub id: String,
    /// Its visible name.
    pub name: String,
    /// GitHub's field data type, such as `SINGLE_SELECT` or `DATE`.
    pub data_type: String,
    /// Ordered choices for a single-select field.
    pub options: Vec<ProjectFieldOption>,
}

/// One configured choice in a Project single-select field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFieldOption {
    /// The option id required when updating an item.
    pub id: String,
    /// Its visible name.
    pub name: String,
    /// GitHub's semantic colour name.
    pub color: String,
}

/// A value set on one Project item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFieldValue {
    /// The field's global id.
    pub field_id: String,
    /// The field's visible name.
    pub field_name: String,
    /// The value in the shape needed by native Project layouts.
    pub value: ProjectValue,
}

/// A custom Project field value supported by the native views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectValue {
    /// One choice from a configured list.
    SingleSelect(String),
    /// A calendar date in ISO 8601 form.
    Date(String),
    /// An iteration, including the span it occupies on a roadmap.
    Iteration {
        /// Its visible title.
        title: String,
        /// Its first day in ISO 8601 form.
        start_date: String,
        /// Its length in days.
        duration: u64,
    },
    /// Free-form text.
    Text(String),
    /// A numeric value.
    Number(String),
    /// One or more names from actor, label, milestone or repository fields.
    Names(Vec<String>),
}

/// One saved view of a Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    /// The view's global id.
    pub id: String,
    /// Its visible name.
    pub name: String,
    /// Its stable number inside the project.
    pub number: u64,
    /// How GitHub lays the view out.
    pub layout: ProjectViewLayout,
    /// The Project filter expression, when one is configured.
    pub filter: Option<String>,
    /// The field used for horizontal groups.
    pub group_by: Option<String>,
    /// The field used as board columns.
    pub vertical_group_by: Option<String>,
    /// Visible field ids, in their saved order.
    pub visible_fields: Vec<String>,
}

/// A Project together with every item in its default ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectBoard {
    /// The project being read.
    pub project: Project,
    /// Its items in project position order.
    pub items: Vec<ProjectItem>,
    /// Fields and their configured choices.
    pub fields: Vec<ProjectField>,
    /// Saved table, board and roadmap views in GitHub order.
    pub views: Vec<ProjectView>,
}

/// One page of a Project board.
///
/// The first page carries fields and saved views; later pages may leave those
/// collections empty because callers merge them into the first page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPage {
    /// The items and any metadata returned on this page.
    pub board: ProjectBoard,
    /// GitHub's cursor for the following page, or `None` at the end.
    pub next_cursor: Option<String>,
}

/// An item's place in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMembership {
    /// The project.
    pub project_id: String,
    /// The project's title.
    pub title: String,
    /// The item's id *within* the project, which removing it takes.
    pub item_id: String,
}

/// One step of a GitHub Actions job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStep {
    /// Its place in the job, from one.
    pub number: u64,
    /// Its name: the step's `name`, or `Run …` when it had none.
    pub name: String,
    /// What it came to.
    pub state: CheckState,
    /// When it started, when it did.
    pub started_at: Option<DateTime<Utc>>,
    /// When it finished, when it did.
    pub completed_at: Option<DateTime<Utc>>,
}

impl JobStep {
    /// How long it took, as `34s` or `1m 19s`; empty while it runs or
    /// when it never ran.
    pub fn duration(&self) -> String {
        let (Some(start), Some(end)) = (self.started_at, self.completed_at) else {
            return String::new();
        };
        let seconds = (end - start).num_seconds().max(0);
        if seconds >= 60 {
            format!("{}m {}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        }
    }
}

/// A GitHub Actions job: what its steps were and how each went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// GitHub's id, which the log is fetched by.
    pub id: u64,
    /// The workflow run it belongs to. What `gh run view` takes.
    #[serde(default)]
    pub run_id: u64,
    /// Its name.
    pub name: String,
    /// What it came to.
    pub state: CheckState,
    /// Its steps, in order.
    pub steps: Vec<JobStep>,
    /// Where it lives on the web.
    pub html_url: String,
}

/// Which side of a diff a review comment sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// The old file: a removed or context line.
    Left,
    /// The new file: an added or context line.
    Right,
}

impl Side {
    /// The value GitHub's `side` field takes.
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }

    /// Map GitHub's `side`.
    pub fn parse(side: &str) -> Self {
        if side == "LEFT" {
            Self::Left
        } else {
            Self::Right
        }
    }
}

/// A comment on a line of a pull's diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    /// GitHub's id.
    pub id: u64,
    /// The file.
    pub path: String,
    /// The line in the current diff, or `None` when the code it was on has
    /// since changed — "outdated", as GitHub says.
    pub line: Option<u32>,
    /// The first line, when the comment is on a range that ends at `line`.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// Which side of the diff.
    pub side: Side,
    /// Who wrote it.
    pub author: User,
    /// When.
    pub created_at: DateTime<Utc>,
    /// The markdown.
    pub body: String,
    /// Where it lives on the web.
    pub html_url: String,
}

/// One entry of an item's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// GitHub's id.
    pub id: u64,
    /// Who wrote it.
    pub author: User,
    /// When.
    pub created_at: DateTime<Utc>,
    /// The markdown.
    pub body: String,
    /// Where it lives on the web.
    pub html_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: Kind, status: Status) -> Item {
        Item {
            repo: RepoId::new("o", "r"),
            number: 1,
            node_id: String::new(),
            title: String::new(),
            kind,
            status,
            author: User {
                login: "a".into(),
                avatar_url: String::new(),
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            comments: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            requested_reviewers: Vec::new(),
            html_url: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn a_merged_pull_is_merged_even_though_the_wire_says_closed() {
        let pull = item(
            Kind::Pull {
                draft: false,
                merged: true,
            },
            Status::Closed,
        );
        assert_eq!(pull.state(), State::Merged);
    }

    #[test]
    fn a_closed_pull_that_was_not_merged_is_closed() {
        let pull = item(
            Kind::Pull {
                draft: false,
                merged: false,
            },
            Status::Closed,
        );
        assert_eq!(pull.state(), State::Closed);
    }

    #[test]
    fn a_draft_is_only_a_draft_while_it_is_open() {
        let open = item(
            Kind::Pull {
                draft: true,
                merged: false,
            },
            Status::Open,
        );
        assert_eq!(open.state(), State::Draft);
        let closed = item(
            Kind::Pull {
                draft: true,
                merged: false,
            },
            Status::Closed,
        );
        assert_eq!(closed.state(), State::Closed);
    }

    #[test]
    fn an_issue_is_open_or_closed_and_nothing_else() {
        assert_eq!(item(Kind::Issue, Status::Open).state(), State::Open);
        assert_eq!(item(Kind::Issue, Status::Closed).state(), State::Closed);
    }

    #[test]
    fn repo_ids_parse_from_the_shapes_github_sends() {
        assert_eq!(
            RepoId::parse("bokuweb/e1"),
            Some(RepoId::new("bokuweb", "e1"))
        );
        assert_eq!(RepoId::parse("bokuweb"), None);
        assert_eq!(RepoId::parse("a/b/c"), None);
        assert_eq!(RepoId::parse("/b"), None);
        assert_eq!(
            RepoId::from_api_url("https://api.github.com/repos/bokuweb/e1/pulls/12"),
            Some(RepoId::new("bokuweb", "e1"))
        );
        assert_eq!(
            RepoId::from_api_url("https://api.github.com/repos/bokuweb/e1"),
            Some(RepoId::new("bokuweb", "e1"))
        );
        assert_eq!(RepoId::from_api_url("https://api.github.com/user"), None);
    }

    #[test]
    fn a_notification_knows_where_its_subject_lives_only_for_kinds_with_a_web_path() {
        let mut notification = Notification {
            id: "1".into(),
            unread: true,
            reason: "mention".into(),
            updated_at: Utc::now(),
            repo: RepoId::new("bokuweb", "e1"),
            title: "t".into(),
            kind: SubjectKind::PullRequest,
            number: Some(7),
        };
        assert_eq!(
            notification.html_url().as_deref(),
            Some("https://github.com/bokuweb/e1/pull/7")
        );
        notification.kind = SubjectKind::Issue;
        assert_eq!(
            notification.html_url().as_deref(),
            Some("https://github.com/bokuweb/e1/issues/7")
        );
        notification.kind = SubjectKind::Commit;
        assert_eq!(notification.html_url(), None);
    }
}

/// What happened to a file in a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
    /// New in this pull.
    Added,
    /// Deleted by this pull.
    Removed,
    /// Changed in place.
    Modified,
    /// Moved, possibly changed too.
    Renamed,
    /// Anything else GitHub reports (`copied`, `changed`, `unchanged`).
    Other,
}

impl FileStatus {
    /// Map GitHub's `status`.
    pub fn parse(status: &str) -> Self {
        match status {
            "added" => Self::Added,
            "removed" => Self::Removed,
            "modified" => Self::Modified,
            "renamed" => Self::Renamed,
            _ => Self::Other,
        }
    }

    /// The one-letter mark a file list leads with.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Removed => "D",
            Self::Modified => "M",
            Self::Renamed => "R",
            Self::Other => "·",
        }
    }
}

/// One file of a pull request's diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullFile {
    /// The path after the pull.
    pub filename: String,
    /// The path before, when it moved.
    pub previous_filename: Option<String>,
    /// What happened to it.
    pub status: FileStatus,
    /// Lines added.
    pub additions: u64,
    /// Lines removed.
    pub deletions: u64,
    /// The unified diff, without the `---`/`+++` header. `None` for a
    /// binary file or one too large for GitHub to send.
    pub patch: Option<String>,
}

/// One commit, as a history row needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// The full hash.
    pub sha: String,
    /// The whole message, subject and body.
    pub message: String,
    /// Who wrote it, as git recorded it. Not every commit has a GitHub
    /// account behind it, and the name in the commit is what git shows.
    pub author_name: String,
    /// The GitHub account, when GitHub matched one to the address.
    pub author: Option<User>,
    /// When it was authored.
    pub authored_at: DateTime<Utc>,
    /// Its parents, first one first. Two or more means a merge.
    pub parents: Vec<String>,
    /// Where it lives on the web.
    pub html_url: String,
}

impl Commit {
    /// The first line, which is what a row shows.
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }

    /// The message under the subject, blank line dropped.
    pub fn body(&self) -> &str {
        match self.message.split_once('\n') {
            Some((_, rest)) => rest.trim_start_matches('\n').trim_end(),
            None => "",
        }
    }

    /// The hash as it is written in prose.
    pub fn short(&self) -> &str {
        let end = self.sha.len().min(7);
        &self.sha[..end]
    }

    /// Whether it brought two histories together.
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// A commit with what it changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitDetail {
    /// The commit itself.
    pub commit: Commit,
    /// Lines added across every file.
    pub additions: u64,
    /// Lines removed across every file.
    pub deletions: u64,
    /// The files it touched, with their patches.
    pub files: Vec<PullFile>,
}

/// Whether a tree entry is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A file.
    Blob,
    /// A directory.
    Tree,
    /// A submodule or a symlink: listed, not opened.
    Other,
}

/// One path in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    /// The path from the repository root.
    pub path: String,
    /// File or directory.
    pub kind: EntryKind,
    /// Bytes, for a file.
    pub size: Option<u64>,
}

/// A repository's whole tree at its default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    /// Every path, files and directories, in GitHub's order.
    pub entries: Vec<TreeEntry>,
    /// GitHub stops listing very large repositories part way through and
    /// says so; the finder tells the reader.
    pub truncated: bool,
}

impl Tree {
    /// The files only, which is what a finder searches.
    pub fn files(&self) -> impl Iterator<Item = &TreeEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Blob)
    }
}

/// A file, read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContent {
    /// The path.
    pub path: String,
    /// Bytes.
    pub size: u64,
    /// The text, when the file is text and small enough for GitHub to
    /// send inline. `None` for a binary file or one over GitHub's limit.
    pub text: Option<String>,
    /// Where it lives on the web, for the cases `text` cannot cover.
    pub html_url: String,
}
