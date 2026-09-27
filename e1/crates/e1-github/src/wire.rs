//! GitHub's JSON, and the mapping into the model.
//!
//! Private: the wire's shapes stop here. Every struct is what one endpoint
//! sends, with only the fields this app reads, and the `into_*` methods are
//! where the rules that are not on the wire are written — a merged pull is
//! `closed` with `merged_at` set; an issues listing contains pulls; a search
//! hit names its repository by URL.

use crate::model::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct WireUser {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl From<WireUser> for User {
    fn from(user: WireUser) -> Self {
        Self {
            login: user.login,
            avatar_url: user.avatar_url,
        }
    }
}

impl From<WireUser> for Viewer {
    fn from(user: WireUser) -> Self {
        Self {
            login: user.login,
            name: user.name,
            avatar_url: user.avatar_url,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireRepo {
    pub full_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub default_branch: String,
    #[serde(default)]
    pub stargazers_count: u64,
    #[serde(default)]
    pub open_issues_count: u64,
    #[serde(default)]
    pub pushed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub html_url: String,
}

impl WireRepo {
    /// `None` when `full_name` is not `owner/name`, which would be a GitHub
    /// bug; dropping the row beats keying a cache by garbage.
    pub fn into_repo(self) -> Option<Repo> {
        let id = RepoId::parse(&self.full_name)?;
        Some(Repo {
            id,
            description: self.description.filter(|text| !text.is_empty()),
            private: self.private,
            default_branch: self.default_branch,
            stars: self.stargazers_count,
            open_issues: self.open_issues_count,
            pushed_at: self.pushed_at,
            html_url: self.html_url,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireLabel {
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl From<WireLabel> for Label {
    fn from(label: WireLabel) -> Self {
        Self {
            name: label.name,
            color: label.color,
            description: label.description.filter(|text| !text.is_empty()),
        }
    }
}

/// The marker an issues listing or a search hit carries when the "issue" is
/// a pull request.
#[derive(Debug, Deserialize)]
pub(crate) struct WirePullMarker {
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
}

/// An issue-shaped object: `/issues`, `/issues/{n}` and `/search/issues` all
/// send this, for pulls as well as issues.
#[derive(Debug, Deserialize)]
pub(crate) struct WireIssue {
    pub number: u64,
    #[serde(default)]
    pub node_id: String,
    pub title: String,
    pub state: String,
    pub user: WireUser,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: Option<u64>,
    #[serde(default)]
    pub labels: Vec<WireLabel>,
    #[serde(default)]
    pub assignees: Vec<WireUser>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub pull_request: Option<WirePullMarker>,
    #[serde(default)]
    pub draft: Option<bool>,
    #[serde(default)]
    pub repository_url: Option<String>,
}

impl WireIssue {
    /// Whether this issue-shaped object is a pull request.
    pub fn is_pull(&self) -> bool {
        self.pull_request.is_some()
    }

    /// The repository, from `repository_url` when the endpoint sent one
    /// (search does) and from the caller otherwise.
    pub fn repo(&self, fallback: Option<&RepoId>) -> Option<RepoId> {
        self.repository_url
            .as_deref()
            .and_then(RepoId::from_api_url)
            .or_else(|| fallback.cloned())
    }

    /// Into the model. `None` when the repository cannot be named.
    pub fn into_item(self, fallback: Option<&RepoId>) -> Option<Item> {
        let repo = self.repo(fallback)?;
        let kind = match &self.pull_request {
            Some(marker) => Kind::Pull {
                draft: self.draft.unwrap_or(false),
                merged: marker.merged_at.is_some(),
            },
            None => Kind::Issue,
        };
        Some(Item {
            repo,
            number: self.number,
            node_id: self.node_id,
            title: self.title,
            kind,
            status: parse_status(&self.state),
            author: self.user.into(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            comments: self.comments,
            labels: self.labels.into_iter().map(Into::into).collect(),
            assignees: self.assignees.into_iter().map(Into::into).collect(),
            requested_reviewers: Vec::new(),
            html_url: self.html_url,
            body: self.body.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireRef {
    #[serde(rename = "ref")]
    pub name: String,
    #[serde(default)]
    pub sha: String,
}

/// What `/pulls` and `/pulls/{n}` send. The listing omits the counts.
#[derive(Debug, Deserialize)]
pub(crate) struct WirePull {
    pub number: u64,
    #[serde(default)]
    pub node_id: String,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
    pub user: WireUser,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub head: WireRef,
    pub base: WireRef,
    #[serde(default)]
    pub labels: Vec<WireLabel>,
    #[serde(default)]
    pub assignees: Vec<WireUser>,
    #[serde(default)]
    pub requested_reviewers: Vec<WireUser>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub comments: Option<u64>,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub changed_files: Option<u64>,
    #[serde(default)]
    pub mergeable: Option<bool>,
}

impl WirePull {
    /// The shared part.
    pub fn into_item(self, repo: &RepoId) -> Item {
        Item {
            repo: repo.clone(),
            number: self.number,
            node_id: self.node_id,
            title: self.title,
            kind: Kind::Pull {
                draft: self.draft,
                merged: self.merged_at.is_some(),
            },
            status: parse_status(&self.state),
            author: self.user.into(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            comments: self.comments,
            labels: self.labels.into_iter().map(Into::into).collect(),
            assignees: self.assignees.into_iter().map(Into::into).collect(),
            requested_reviewers: self
                .requested_reviewers
                .into_iter()
                .map(Into::into)
                .collect(),
            html_url: self.html_url,
            body: self.body.unwrap_or_default(),
        }
    }

    /// The whole pull. The counts default to zero when the listing omitted
    /// them; a caller wanting them asks for the pull by number.
    pub fn into_pull(self, repo: &RepoId) -> Pull {
        let head = self.head.name.clone();
        let base = self.base.name.clone();
        let additions = self.additions.unwrap_or(0);
        let deletions = self.deletions.unwrap_or(0);
        let changed_files = self.changed_files.unwrap_or(0);
        let mergeable = self.mergeable;
        let head_sha = self.head.sha.clone();
        Pull {
            item: self.into_item(repo),
            head,
            base,
            additions,
            deletions,
            changed_files,
            mergeable,
            head_sha,
        }
    }
}

/// What `/commits/{sha}/check-runs` sends.
#[derive(Debug, Deserialize)]
pub(crate) struct WireCheckRuns {
    #[serde(default)]
    pub check_runs: Vec<WireCheckRun>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireApp {
    #[serde(default)]
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireCheckRun {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub app: Option<WireApp>,
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
}

impl From<WireCheckRun> for CheckRun {
    fn from(run: WireCheckRun) -> Self {
        let state = if run.status != "completed" {
            CheckState::Pending
        } else {
            match run.conclusion.as_deref() {
                Some("success") => CheckState::Success,
                Some("neutral") | Some("skipped") => CheckState::Neutral,
                _ => CheckState::Failure,
            }
        };
        Self {
            id: run.id,
            actions: run
                .app
                .as_ref()
                .is_some_and(|app| app.slug == "github-actions"),
            name: run.name,
            state,
            html_url: run.html_url,
        }
    }
}

/// What `/actions/jobs/{id}` sends.
#[derive(Debug, Deserialize)]
pub(crate) struct WireJob {
    pub id: u64,
    #[serde(default)]
    pub run_id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub steps: Vec<WireJobStep>,
    #[serde(default)]
    pub html_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireJobStep {
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

/// GitHub's `status` + `conclusion` pair, as one state.
fn check_state(status: &str, conclusion: Option<&str>) -> CheckState {
    if status != "completed" {
        CheckState::Pending
    } else {
        match conclusion {
            Some("success") => CheckState::Success,
            Some("neutral") | Some("skipped") => CheckState::Neutral,
            _ => CheckState::Failure,
        }
    }
}

impl From<WireJob> for Job {
    fn from(job: WireJob) -> Self {
        Self {
            id: job.id,
            run_id: job.run_id,
            name: job.name,
            state: check_state(&job.status, job.conclusion.as_deref()),
            steps: job
                .steps
                .into_iter()
                .map(|step| JobStep {
                    number: step.number,
                    name: step.name,
                    state: check_state(&step.status, step.conclusion.as_deref()),
                    started_at: step.started_at,
                    completed_at: step.completed_at,
                })
                .collect(),
            html_url: job.html_url,
        }
    }
}

/// What `/commits` and `/commits/{sha}` send.
#[derive(Debug, Deserialize)]
pub(crate) struct WireCommit {
    pub sha: String,
    pub commit: WireCommitBody,
    /// The GitHub account, when GitHub matched one; `null` otherwise.
    #[serde(default)]
    pub author: Option<WireUser>,
    #[serde(default)]
    pub parents: Vec<WireCommitParent>,
    #[serde(default)]
    pub html_url: String,
    /// Only `/commits/{sha}` sends these.
    #[serde(default)]
    pub stats: Option<WireCommitStats>,
    #[serde(default)]
    pub files: Vec<WirePullFile>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireCommitBody {
    #[serde(default)]
    pub message: String,
    pub author: Option<WireCommitAuthor>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireCommitAuthor {
    #[serde(default)]
    pub name: String,
    pub date: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireCommitParent {
    pub sha: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireCommitStats {
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
}

impl WireCommit {
    /// The row's worth of it.
    pub(crate) fn into_commit(self) -> Commit {
        let (name, date) = match self.commit.author {
            Some(author) => (author.name, author.date),
            None => (String::new(), None),
        };
        Commit {
            sha: self.sha,
            message: self.commit.message,
            author_name: name,
            author: self.author.map(Into::into),
            authored_at: date.unwrap_or_else(Utc::now),
            parents: self.parents.into_iter().map(|parent| parent.sha).collect(),
            html_url: self.html_url,
        }
    }
}

impl From<WireCommit> for CommitDetail {
    fn from(mut wire: WireCommit) -> Self {
        let (additions, deletions) = match &wire.stats {
            Some(stats) => (stats.additions, stats.deletions),
            None => (0, 0),
        };
        let files = std::mem::take(&mut wire.files)
            .into_iter()
            .map(PullFile::from)
            .collect();
        Self {
            commit: wire.into_commit(),
            additions,
            deletions,
            files,
        }
    }
}

/// What `/pulls/{n}/comments` sends: a comment on a line of the diff.
#[derive(Debug, Deserialize)]
pub(crate) struct WireReviewComment {
    pub id: u64,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub start_line: Option<u32>,
    #[serde(default)]
    pub side: Option<String>,
    pub user: WireUser,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub html_url: String,
}

impl From<WireReviewComment> for ReviewComment {
    fn from(comment: WireReviewComment) -> Self {
        Self {
            id: comment.id,
            path: comment.path,
            line: comment.line,
            start_line: comment.start_line,
            side: Side::parse(comment.side.as_deref().unwrap_or("RIGHT")),
            author: comment.user.into(),
            created_at: comment.created_at,
            body: comment.body,
            html_url: comment.html_url,
        }
    }
}

/// What `/commits/{sha}/status` sends: the older kind of check.
#[derive(Debug, Deserialize)]
pub(crate) struct WireCombinedStatus {
    #[serde(default)]
    pub statuses: Vec<WireStatus>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireStatus {
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub target_url: Option<String>,
}

impl From<WireStatus> for CheckRun {
    fn from(status: WireStatus) -> Self {
        let state = match status.state.as_str() {
            "success" => CheckState::Success,
            "pending" => CheckState::Pending,
            _ => CheckState::Failure,
        };
        Self {
            id: 0,
            actions: false,
            name: status.context,
            state,
            html_url: status.target_url,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireSubject {
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireRepoRef {
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireNotification {
    pub id: String,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub reason: String,
    pub updated_at: DateTime<Utc>,
    pub subject: WireSubject,
    pub repository: WireRepoRef,
}

impl WireNotification {
    /// Into the model. `None` when the repository cannot be named.
    pub fn into_notification(self) -> Option<Notification> {
        let repo = RepoId::parse(&self.repository.full_name)?;
        let number = self
            .subject
            .url
            .as_deref()
            .and_then(|url| url.rsplit('/').next())
            .and_then(|tail| tail.parse().ok());
        Some(Notification {
            id: self.id,
            unread: self.unread,
            reason: self.reason,
            updated_at: self.updated_at,
            repo,
            title: self.subject.title,
            kind: SubjectKind::parse(&self.subject.kind),
            number,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireComment {
    pub id: u64,
    pub user: WireUser,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub html_url: String,
}

impl From<WireComment> for Comment {
    fn from(comment: WireComment) -> Self {
        Self {
            id: comment.id,
            author: comment.user.into(),
            created_at: comment.created_at,
            body: comment.body,
            html_url: comment.html_url,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireSearch {
    #[serde(default)]
    pub items: Vec<WireIssue>,
}

/// One entry of `/pulls/{n}/files`.
#[derive(Debug, Deserialize)]
pub(crate) struct WirePullFile {
    pub filename: String,
    #[serde(default)]
    pub previous_filename: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub patch: Option<String>,
}

impl From<WirePullFile> for PullFile {
    fn from(file: WirePullFile) -> Self {
        Self {
            filename: file.filename,
            previous_filename: file.previous_filename,
            status: FileStatus::parse(&file.status),
            additions: file.additions,
            deletions: file.deletions,
            patch: file.patch,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireTreeEntry {
    pub path: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub size: Option<u64>,
}

/// What `/git/trees/{ref}?recursive=1` sends.
#[derive(Debug, Deserialize)]
pub(crate) struct WireTree {
    #[serde(default)]
    pub tree: Vec<WireTreeEntry>,
    #[serde(default)]
    pub truncated: bool,
}

impl From<WireTree> for Tree {
    fn from(tree: WireTree) -> Self {
        Self {
            entries: tree
                .tree
                .into_iter()
                .map(|entry| TreeEntry {
                    kind: match entry.kind.as_str() {
                        "blob" => EntryKind::Blob,
                        "tree" => EntryKind::Tree,
                        _ => EntryKind::Other,
                    },
                    path: entry.path,
                    size: entry.size,
                })
                .collect(),
            truncated: tree.truncated,
        }
    }
}

/// What `/contents/{path}` sends for a file.
#[derive(Debug, Deserialize)]
pub(crate) struct WireContents {
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub html_url: String,
}

impl WireContents {
    /// Decode what GitHub sent. Base64 with newlines in it, or nothing at
    /// all above a megabyte; either way a file that is not UTF-8 is binary
    /// and is offered on the web instead.
    pub fn into_file(self) -> FileContent {
        use base64::Engine as _;
        let text = match (self.encoding.as_deref(), self.content.as_deref()) {
            (Some("base64"), Some(content)) if !content.is_empty() => {
                let stripped: String = content.chars().filter(|c| !c.is_whitespace()).collect();
                base64::engine::general_purpose::STANDARD
                    .decode(stripped)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            }
            _ => None,
        };
        FileContent {
            path: self.path,
            size: self.size,
            text,
            html_url: self.html_url,
        }
    }
}

/// GitHub's `message` on an error body.
#[derive(Debug, Deserialize)]
pub(crate) struct WireMessage {
    #[serde(default)]
    pub message: String,
}

fn parse_status(state: &str) -> Status {
    match state {
        "open" => Status::Open,
        _ => Status::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PULL_AS_ISSUE: &str = r#"{
        "number": 12, "title": "Add a thing", "state": "closed",
        "user": {"login": "alice", "avatar_url": "https://a"},
        "created_at": "2026-09-01T10:00:00Z", "updated_at": "2026-09-02T10:00:00Z",
        "comments": 3, "labels": [{"name": "bug", "color": "d73a4a"}],
        "html_url": "https://github.com/o/r/pull/12", "body": null,
        "pull_request": {"merged_at": "2026-09-02T10:00:00Z"},
        "repository_url": "https://api.github.com/repos/o/r"
    }"#;

    #[test]
    fn a_search_hit_names_its_repository_and_knows_it_is_a_merged_pull() {
        let wire: WireIssue = serde_json::from_str(PULL_AS_ISSUE).unwrap();
        assert!(wire.is_pull());
        let item = wire.into_item(None).unwrap();
        assert_eq!(item.repo, RepoId::new("o", "r"));
        assert_eq!(item.state(), State::Merged);
        assert_eq!(item.comments, Some(3));
        assert_eq!(item.labels[0].color, "d73a4a");
        assert_eq!(item.body, "", "a null body is an empty description");
    }

    #[test]
    fn an_issue_from_a_listing_takes_the_repository_it_was_listed_under() {
        let json = r#"{
            "number": 3, "title": "Crash", "state": "open",
            "user": {"login": "bob"},
            "created_at": "2026-09-01T10:00:00Z", "updated_at": "2026-09-01T10:00:00Z"
        }"#;
        let wire: WireIssue = serde_json::from_str(json).unwrap();
        assert!(!wire.is_pull());
        let repo = RepoId::new("o", "r");
        let item = wire.into_item(Some(&repo)).unwrap();
        assert_eq!(item.repo, repo);
        assert_eq!(item.kind, Kind::Issue);
        assert_eq!(item.state(), State::Open);
        assert_eq!(item.comments, None);
    }

    #[test]
    fn a_pull_listing_carries_no_counts_and_the_detail_does() {
        let listed = r#"{
            "number": 5, "title": "T", "state": "open", "draft": true,
            "user": {"login": "carol"},
            "created_at": "2026-09-01T10:00:00Z", "updated_at": "2026-09-01T10:00:00Z",
            "head": {"ref": "feature"}, "base": {"ref": "main"}
        }"#;
        let wire: WirePull = serde_json::from_str(listed).unwrap();
        let pull = wire.into_pull(&RepoId::new("o", "r"));
        assert_eq!(pull.item.state(), State::Draft);
        assert_eq!(
            (pull.head.as_str(), pull.base.as_str()),
            ("feature", "main")
        );
        assert_eq!(pull.additions, 0);

        let detailed = r#"{
            "number": 5, "title": "T", "state": "closed", "merged_at": "2026-09-03T00:00:00Z",
            "user": {"login": "carol"},
            "created_at": "2026-09-01T10:00:00Z", "updated_at": "2026-09-01T10:00:00Z",
            "head": {"ref": "feature"}, "base": {"ref": "main"},
            "additions": 40, "deletions": 2, "changed_files": 3, "mergeable": null,
            "requested_reviewers": [{"login": "dave"}]
        }"#;
        let wire: WirePull = serde_json::from_str(detailed).unwrap();
        let pull = wire.into_pull(&RepoId::new("o", "r"));
        assert_eq!(pull.item.state(), State::Merged);
        assert_eq!(
            (pull.additions, pull.deletions, pull.changed_files),
            (40, 2, 3)
        );
        assert_eq!(pull.mergeable, None);
        assert_eq!(pull.item.requested_reviewers[0].login, "dave");
    }

    #[test]
    fn a_notification_reads_the_number_off_the_subject_url() {
        let json = r#"{
            "id": "123", "unread": true, "reason": "review_requested",
            "updated_at": "2026-09-01T10:00:00Z",
            "subject": {"title": "Fix", "url": "https://api.github.com/repos/o/r/pulls/44", "type": "PullRequest"},
            "repository": {"full_name": "o/r"}
        }"#;
        let wire: WireNotification = serde_json::from_str(json).unwrap();
        let notification = wire.into_notification().unwrap();
        assert_eq!(notification.number, Some(44));
        assert_eq!(notification.kind, SubjectKind::PullRequest);
        assert_eq!(notification.repo, RepoId::new("o", "r"));

        let json = r#"{
            "id": "124", "unread": false, "reason": "subscribed",
            "updated_at": "2026-09-01T10:00:00Z",
            "subject": {"title": "v1.0", "url": null, "type": "Release"},
            "repository": {"full_name": "o/r"}
        }"#;
        let wire: WireNotification = serde_json::from_str(json).unwrap();
        let notification = wire.into_notification().unwrap();
        assert_eq!(notification.number, None);
        assert_eq!(notification.kind, SubjectKind::Release);
    }

    #[test]
    fn a_binary_file_has_no_patch_and_a_rename_keeps_where_it_came_from() {
        let json = r#"[
            {"filename": "a.png", "status": "added", "additions": 0, "deletions": 0},
            {"filename": "src/new.rs", "previous_filename": "src/old.rs", "status": "renamed",
             "additions": 1, "deletions": 1, "patch": "@@ -1 +1 @@\n-a\n+b"}
        ]"#;
        let files: Vec<WirePullFile> = serde_json::from_str(json).unwrap();
        let files: Vec<PullFile> = files.into_iter().map(Into::into).collect();
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[0].patch, None);
        assert_eq!(files[1].status, FileStatus::Renamed);
        assert_eq!(files[1].previous_filename.as_deref(), Some("src/old.rs"));
        assert!(files[1].patch.as_deref().unwrap().starts_with("@@"));
    }

    #[test]
    fn a_tree_keeps_files_and_directories_apart_and_says_when_it_was_cut_short() {
        let json = r#"{"tree":[{"path":"src","type":"tree"},{"path":"src/main.rs","type":"blob","size":12}],"truncated":true}"#;
        let tree: Tree = serde_json::from_str::<WireTree>(json).unwrap().into();
        assert!(tree.truncated);
        let files: Vec<_> = tree.files().collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].size, Some(12));
    }

    #[test]
    fn a_file_is_decoded_from_base64_with_newlines_and_a_binary_one_is_not_text() {
        let json = r#"{"path":"a.txt","size":11,"encoding":"base64","content":"aGVsbG8g\nd29ybGQ=\n","html_url":"https://github.com/o/r/blob/main/a.txt"}"#;
        let file = serde_json::from_str::<WireContents>(json)
            .unwrap()
            .into_file();
        assert_eq!(file.text.as_deref(), Some("hello world"));

        let json =
            r#"{"path":"a.png","size":3,"encoding":"base64","content":"/9j/","html_url":""}"#;
        let file = serde_json::from_str::<WireContents>(json)
            .unwrap()
            .into_file();
        assert_eq!(file.text, None, "not utf-8");

        let json =
            r#"{"path":"big.bin","size":5000000,"encoding":"none","content":"","html_url":""}"#;
        let file = serde_json::from_str::<WireContents>(json)
            .unwrap()
            .into_file();
        assert_eq!(file.text, None, "too large for the contents endpoint");
    }

    #[test]
    fn a_repository_with_a_bad_name_is_dropped_rather_than_keyed_by_garbage() {
        let json = r#"{"full_name": "nonsense"}"#;
        let wire: WireRepo = serde_json::from_str(json).unwrap();
        assert!(wire.into_repo().is_none());
        let json = r#"{"full_name": "o/r", "description": "", "private": true, "pushed_at": "2026-09-01T10:00:00Z"}"#;
        let repo: WireRepo = serde_json::from_str(json).unwrap();
        let repo = repo.into_repo().unwrap();
        assert!(repo.private);
        assert_eq!(repo.description, None, "an empty description is none");
    }
}
