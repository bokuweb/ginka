//! GitHub, as this app sees it.
//!
//! The data model, the [`GitHub`] trait every view reaches the network
//! through, its REST implementation, the token discovery that feeds it, and a
//! scripted fake for tests and for running without a network. Nothing here
//! knows about GPUI: this crate is what a host that owns its own state would
//! implement the trait against (`docs/roadmap.md` §4.3, E2).

pub mod auth;
pub mod cache;
pub mod model;
pub mod rest;
pub mod scripted;
mod wire;

pub use auth::{Source, Token};
pub use cache::HttpCache;
use std::collections::HashMap;

pub use model::*;
pub use rest::{PAGE_SIZE, Rest};
pub use scripted::Scripted;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Why a request did not produce an answer.
///
/// Typed rather than stringly so a view can tell "sign in" from "try again
/// later" from "this is a bug": the first two are states it draws, the third
/// is a log line.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Nothing supplied a token; see [`auth::discover`] for where one is
    /// looked for.
    #[error("no GitHub token: set E1_GITHUB_TOKEN or GITHUB_TOKEN, or run `gh auth login`")]
    NoToken,
    /// GitHub answered with a status this app does not treat as success.
    #[error("GitHub answered {status} for {path}: {message}")]
    Status {
        /// The HTTP status.
        status: u16,
        /// The request path, without the host.
        path: String,
        /// GitHub's own `message`, when the body carried one.
        message: String,
    },
    /// The token's rate limit is exhausted until `reset`.
    #[error("rate limited by GitHub")]
    RateLimited {
        /// When the window opens again, when GitHub said.
        reset: Option<DateTime<Utc>>,
    },
    /// The request never got an answer: DNS, TLS, a timeout.
    #[error("could not reach GitHub: {0}")]
    Transport(String),
    /// GitHub answered, and the answer was not the shape this app expects.
    #[error("could not read GitHub's answer: {0}")]
    Decode(String),
    /// The implementation in use cannot do this. A host that proxies through
    /// its own daemon may lag behind the trait, and a view must be able to
    /// draw that rather than panic.
    #[error("this GitHub source cannot {0}")]
    Unsupported(&'static str),
}

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Which of a repository's lists is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ListKind {
    /// Pull requests.
    Pulls,
    /// Issues, with pull requests filtered out: GitHub's issues endpoint
    /// returns both, and a list that said "issues" and showed pulls would
    /// be lying.
    Issues,
}

/// Which items of a list are wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StatusFilter {
    /// Open items only, which is what a list shows first.
    #[default]
    Open,
    /// Closed items only, merged pulls included.
    Closed,
    /// Everything.
    All,
}

impl StatusFilter {
    /// The value GitHub's `state` query parameter takes.
    pub fn as_query(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::All => "all",
        }
    }
}

/// Everything a view may ask of GitHub.
///
/// Blocking, and `Send + Sync`: views call it on the background executor and
/// never on the UI thread. Blocking rather than async is deliberate (roadmap
/// E2): a host that answers over its own socket can implement a blocking call
/// with `block_on`, whereas an async trait would commit both apps to one
/// executor. Each method is one screen's question, and each returns the model
/// type that screen draws.
pub trait GitHub: Send + Sync {
    /// Who the token belongs to.
    fn viewer(&self) -> Result<Viewer>;
    /// The unread inbox, newest first.
    fn notifications(&self) -> Result<Vec<Notification>>;
    /// The repositories the viewer can reach, most recently pushed first.
    fn repositories(&self) -> Result<Vec<Repo>>;
    /// A repository's pulls or issues, filtered by status, newest first.
    fn items(&self, repo: &RepoId, kind: ListKind, status: StatusFilter) -> Result<Vec<Item>>;
    /// Items matching a GitHub search query (`is:pr review-requested:@me`).
    fn search(&self, query: &str) -> Result<Vec<Item>>;
    /// One item by number, whether it is a pull or an issue.
    fn item(&self, repo: &RepoId, number: u64) -> Result<Item>;
    /// A pull with what only a pull has. `Error::Status(404)` for an issue.
    fn pull(&self, repo: &RepoId, number: u64) -> Result<Pull>;
    /// An item's comments, oldest first.
    fn comments(&self, repo: &RepoId, number: u64) -> Result<Vec<Comment>>;
    /// The files a pull changes, with their diffs.
    ///
    /// Defaulted to [`Error::Unsupported`] so a host implementation that
    /// lags behind the trait still compiles; the view draws that.
    fn pull_files(&self, repo: &RepoId, number: u64) -> Result<Vec<PullFile>> {
        let _ = (repo, number);
        Err(Error::Unsupported("list a pull's files"))
    }
    /// Every path in a repository at its default branch, in one answer.
    fn tree(&self, repo: &RepoId) -> Result<Tree> {
        let _ = repo;
        Err(Error::Unsupported("list a repository's files"))
    }
    /// One file at the default branch.
    fn file(&self, repo: &RepoId, path: &str) -> Result<FileContent> {
        let _ = (repo, path);
        Err(Error::Unsupported("read a file"))
    }
    /// The bytes of an avatar, at a size fit for a row. Not GitHub's API,
    /// but GitHub's picture, and a host that proxies the API proxies this
    /// too.
    fn avatar(&self, url: &str) -> Result<Vec<u8>> {
        let _ = url;
        Err(Error::Unsupported("fetch an avatar"))
    }
    /// Leave a comment on an item. The answer is the comment as GitHub
    /// stored it, with its id and time.
    fn comment_on(&self, repo: &RepoId, number: u64, body: &str) -> Result<Comment> {
        let _ = (repo, number, body);
        Err(Error::Unsupported("comment"))
    }
    /// Close an item, or open it again. Works for pulls as well as issues.
    fn set_open(&self, repo: &RepoId, number: u64, open: bool) -> Result<Item> {
        let _ = (repo, number, open);
        Err(Error::Unsupported("close or reopen"))
    }
    /// Merge a pull, one of three ways. GitHub refuses one that is not
    /// mergeable, or a method the repository does not allow, and says why.
    fn merge(&self, repo: &RepoId, number: u64, method: MergeMethod) -> Result<()> {
        let _ = (repo, number, method);
        Err(Error::Unsupported("merge"))
    }
    /// Every check and status on a commit.
    fn checks(&self, repo: &RepoId, sha: &str) -> Result<Checks> {
        let _ = (repo, sha);
        Err(Error::Unsupported("read checks"))
    }
    /// The comments on a pull's diff, oldest first.
    fn review_comments(&self, repo: &RepoId, number: u64) -> Result<Vec<ReviewComment>> {
        let _ = (repo, number);
        Err(Error::Unsupported("read review comments"))
    }
    /// Comment on a line of a pull's diff, at the pull's head commit — or
    /// on the lines from `start` to `line`, when `start` is given.
    #[allow(clippy::too_many_arguments)]
    fn review_comment(
        &self,
        repo: &RepoId,
        number: u64,
        commit: &str,
        path: &str,
        start: Option<u32>,
        line: u32,
        side: Side,
        body: &str,
    ) -> Result<ReviewComment> {
        let _ = (repo, number, commit, path, start, line, side, body);
        Err(Error::Unsupported("comment on a line"))
    }
    /// How the checks stand on each of several pulls, in one round trip:
    /// what a list shows beside its rows. A pull with no checks is left
    /// out of the answer.
    fn pull_checks(&self, keys: &[(RepoId, u64)]) -> Result<HashMap<(RepoId, u64), CheckState>> {
        let _ = keys;
        Err(Error::Unsupported("read the checks on a list"))
    }
    /// Mark a pull, by its global id, as a draft or as ready for review.
    fn set_draft(&self, node_id: &str, draft: bool) -> Result<()> {
        let _ = (node_id, draft);
        Err(Error::Unsupported("change draft state"))
    }
    /// One page of a repository's commits on its default branch, newest
    /// first. Pages count from one; a short page is the last one.
    fn commits(&self, repo: &RepoId, page: u32) -> Result<Vec<Commit>> {
        let _ = (repo, page);
        Err(Error::Unsupported("read a repository's history"))
    }
    /// One commit, with the files it touched and their patches.
    fn commit(&self, repo: &RepoId, sha: &str) -> Result<CommitDetail> {
        let _ = (repo, sha);
        Err(Error::Unsupported("read a commit"))
    }
    /// A GitHub Actions job: its steps and how each went.
    fn job(&self, repo: &RepoId, job_id: u64) -> Result<Job> {
        let _ = (repo, job_id);
        Err(Error::Unsupported("read a job"))
    }
    /// The log of a GitHub Actions job, as plain text.
    fn job_log(&self, repo: &RepoId, job_id: u64) -> Result<String> {
        let _ = (repo, job_id);
        Err(Error::Unsupported("read a job's log"))
    }
    /// Review a pull: approve it, ask for changes, or only comment.
    fn review(&self, repo: &RepoId, number: u64, event: ReviewEvent, body: &str) -> Result<()> {
        let _ = (repo, number, event, body);
        Err(Error::Unsupported("review"))
    }
    /// The labels a repository has to offer.
    fn labels(&self, repo: &RepoId) -> Result<Vec<Label>> {
        let _ = repo;
        Err(Error::Unsupported("list labels"))
    }
    /// Put labels on an item, keeping the ones it has.
    fn add_labels(&self, repo: &RepoId, number: u64, labels: &[String]) -> Result<Item> {
        let _ = (repo, number, labels);
        Err(Error::Unsupported("add a label"))
    }
    /// Take one label off an item.
    fn remove_label(&self, repo: &RepoId, number: u64, label: &str) -> Result<Item> {
        let _ = (repo, number, label);
        Err(Error::Unsupported("remove a label"))
    }
    /// The people an item in this repository can be assigned to.
    fn assignees(&self, repo: &RepoId) -> Result<Vec<User>> {
        let _ = repo;
        Err(Error::Unsupported("list assignees"))
    }
    /// Assign people to an item, keeping the ones already on it.
    fn add_assignees(&self, repo: &RepoId, number: u64, logins: &[String]) -> Result<Item> {
        let _ = (repo, number, logins);
        Err(Error::Unsupported("assign"))
    }
    /// Take people off an item.
    fn remove_assignees(&self, repo: &RepoId, number: u64, logins: &[String]) -> Result<Item> {
        let _ = (repo, number, logins);
        Err(Error::Unsupported("unassign"))
    }
    /// The projects an owner — a user or an organisation — has.
    fn projects(&self, owner: &str) -> Result<Vec<Project>> {
        let _ = owner;
        Err(Error::Unsupported("list projects"))
    }
    /// Every Project the viewer can reach: their own and those belonging to
    /// organisations they are a member of.
    fn all_projects(&self) -> Result<Vec<Project>> {
        let viewer = self.viewer()?;
        self.projects(&viewer.login)
    }
    /// One Project and the items it contains.
    fn project(&self, project: &Project) -> Result<ProjectBoard> {
        let _ = project;
        Err(Error::Unsupported("read a project"))
    }
    /// One page of a Project. Sources that do not support incremental reads
    /// may return their complete [`ProjectBoard`] as a single page.
    fn project_page(&self, project: &Project, after: Option<&str>) -> Result<ProjectPage> {
        if after.is_some() {
            return Err(Error::Unsupported("page through a project"));
        }
        Ok(ProjectPage {
            board: self.project(project)?,
            next_cursor: None,
        })
    }
    /// Set or clear one single-select field on an item inside a Project.
    fn set_project_single_select(
        &self,
        project_id: &str,
        item_id: &str,
        field_id: &str,
        option_id: Option<&str>,
    ) -> Result<()> {
        let _ = (project_id, item_id, field_id, option_id);
        Err(Error::Unsupported("update a project field"))
    }
    /// The projects an item is in.
    fn item_projects(&self, repo: &RepoId, number: u64) -> Result<Vec<ProjectMembership>> {
        let _ = (repo, number);
        Err(Error::Unsupported("list an item's projects"))
    }
    /// Put an item, by its global id, into a project.
    fn add_to_project(&self, project_id: &str, node_id: &str) -> Result<()> {
        let _ = (project_id, node_id);
        Err(Error::Unsupported("add to a project"))
    }
    /// Take an item out of a project.
    fn remove_from_project(&self, project_id: &str, item_id: &str) -> Result<()> {
        let _ = (project_id, item_id);
        Err(Error::Unsupported("remove from a project"))
    }
}
