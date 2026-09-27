//! A [`GitHub`] that answers from memory.
//!
//! What tests are written against, and what `E1_DEMO=1` runs the window over,
//! so that neither needs a token or a network. It answers the same questions
//! the REST client does, including a small evaluator for the search queries
//! this app generates, so a section that lists "pulls waiting for my review"
//! can be tested end to end.

use crate::model::*;
use crate::{Error, GitHub, ListKind, Result, StatusFilter};
use chrono::{Duration, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

/// What only a pull carries, keyed by item.
#[derive(Debug, Clone)]
struct PullExtra {
    head: String,
    base: String,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    mergeable: Option<bool>,
}

#[derive(Default)]
struct Data {
    viewer: Option<Viewer>,
    repos: Vec<Repo>,
    notifications: Vec<Notification>,
    items: Vec<Item>,
    pulls: HashMap<(RepoId, u64), PullExtra>,
    comments: HashMap<(RepoId, u64), Vec<Comment>>,
    files: HashMap<(RepoId, u64), Vec<PullFile>>,
    trees: HashMap<RepoId, Tree>,
    contents: HashMap<(RepoId, String), String>,
    memberships: Vec<((RepoId, u64), ProjectMembership)>,
    review_comments: Vec<((RepoId, u64), ReviewComment)>,
    /// When set, every call fails with this. For testing the error states.
    failing: Option<String>,
}

/// GitHub from memory.
#[derive(Default)]
pub struct Scripted {
    data: Mutex<Data>,
}

/// Build a user from a login.
pub fn user(login: &str) -> User {
    User {
        login: login.to_string(),
        avatar_url: format!("https://avatars.githubusercontent.com/{login}"),
    }
}

impl Scripted {
    /// Nothing in it. Every listing is empty and there is no viewer.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Who the token is.
    pub fn with_viewer(self, login: &str, name: Option<&str>) -> Self {
        self.data.lock().unwrap().viewer = Some(Viewer {
            login: login.to_string(),
            name: name.map(str::to_string),
            avatar_url: format!("https://avatars.githubusercontent.com/{login}"),
        });
        self
    }

    /// A repository the viewer can reach.
    pub fn with_repo(self, repo: Repo) -> Self {
        self.data.lock().unwrap().repos.push(repo);
        self
    }

    /// An issue or a pull. A pull gets default branch and count details;
    /// use [`Scripted::with_pull`] to say what they are.
    pub fn with_item(self, item: Item) -> Self {
        self.data.lock().unwrap().items.push(item);
        self
    }

    /// A pull with its own details.
    pub fn with_pull(self, pull: Pull) -> Self {
        {
            let mut data = self.data.lock().unwrap();
            data.pulls.insert(
                (pull.item.repo.clone(), pull.item.number),
                PullExtra {
                    head: pull.head,
                    base: pull.base,
                    additions: pull.additions,
                    deletions: pull.deletions,
                    changed_files: pull.changed_files,
                    mergeable: pull.mergeable,
                },
            );
            data.items.push(pull.item);
        }
        self
    }

    /// The comments on an item.
    pub fn with_comments(self, repo: &RepoId, number: u64, comments: Vec<Comment>) -> Self {
        self.data
            .lock()
            .unwrap()
            .comments
            .insert((repo.clone(), number), comments);
        self
    }

    /// The files a pull changes.
    pub fn with_files(self, repo: &RepoId, number: u64, files: Vec<PullFile>) -> Self {
        self.data
            .lock()
            .unwrap()
            .files
            .insert((repo.clone(), number), files);
        self
    }

    /// A repository's files, as paths with sizes.
    pub fn with_tree(self, repo: &RepoId, paths: &[(&str, u64)]) -> Self {
        let mut dirs: Vec<String> = Vec::new();
        let mut entries = Vec::new();
        for (path, size) in paths {
            let mut prefix = String::new();
            for part in path.split('/').take(path.matches('/').count()) {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if !dirs.contains(&prefix) {
                    dirs.push(prefix.clone());
                    entries.push(TreeEntry {
                        path: prefix.clone(),
                        kind: EntryKind::Tree,
                        size: None,
                    });
                }
            }
            entries.push(TreeEntry {
                path: path.to_string(),
                kind: EntryKind::Blob,
                size: Some(*size),
            });
        }
        self.data.lock().unwrap().trees.insert(
            repo.clone(),
            Tree {
                entries,
                truncated: false,
            },
        );
        self
    }

    /// What a file says.
    pub fn with_file(self, repo: &RepoId, path: &str, text: &str) -> Self {
        self.data
            .lock()
            .unwrap()
            .contents
            .insert((repo.clone(), path.to_string()), text.to_string());
        self
    }

    /// An inbox row.
    pub fn with_notification(self, notification: Notification) -> Self {
        self.data.lock().unwrap().notifications.push(notification);
        self
    }

    /// Make every call fail, for testing what a view does with that.
    pub fn failing(self, message: &str) -> Self {
        self.data.lock().unwrap().failing = Some(message.to_string());
        self
    }

    /// A small, plausible account: two repositories, a handful of pulls and
    /// issues in each, an inbox, and comments. What `E1_DEMO=1` shows.
    pub fn sample() -> Self {
        let now = Utc::now();
        let e1 = RepoId::new("bokuweb", "e1");
        let ginka = RepoId::new("bokuweb", "ginka");
        let item = |repo: &RepoId,
                    number: u64,
                    title: &str,
                    kind: Kind,
                    status: Status,
                    author: &str,
                    hours: i64,
                    comments: u64,
                    labels: &[(&str, &str)]| Item {
            repo: repo.clone(),
            number,
            node_id: format!("node-{repo}-{number}"),
            title: title.to_string(),
            kind,
            status,
            author: user(author),
            created_at: now - Duration::hours(hours + 24),
            updated_at: now - Duration::hours(hours),
            comments: Some(comments),
            labels: labels
                .iter()
                .map(|(name, color)| Label {
                    name: name.to_string(),
                    color: color.to_string(),
                    description: None,
                })
                .collect(),
            assignees: Vec::new(),
            requested_reviewers: Vec::new(),
            html_url: format!(
                "https://github.com/{repo}/{}/{number}",
                if matches!(kind, Kind::Pull { .. }) {
                    "pull"
                } else {
                    "issues"
                }
            ),
            body: format!(
                "## Summary\n\nThis is *scripted* data for **{title}**, see [the roadmap](https://github.com/bokuweb/ginka/blob/main/e1/docs/roadmap.md) and `E1_DEMO=1`.\n\n- one thing\n- another thing\n\n| item | count |\n| --- | ---: |\n| pass | 146 |\n| change | 0 |\n\n```rust\nfn main() {{ println!(\"hello\"); }}\n```"
            ),
        };
        let open = Kind::Pull {
            draft: false,
            merged: false,
        };
        let draft = Kind::Pull {
            draft: true,
            merged: false,
        };
        let merged = Kind::Pull {
            draft: false,
            merged: true,
        };
        let pull = |item: Item, head: &str, adds: u64, dels: u64, files: u64| Pull {
            item,
            head: head.to_string(),
            base: "main".to_string(),
            additions: adds,
            deletions: dels,
            changed_files: files,
            mergeable: Some(true),
            head_sha: format!("sha-{head}"),
        };
        let comment = |id: u64, author: &str, hours: i64, body: &str| Comment {
            id,
            author: user(author),
            created_at: now - Duration::hours(hours),
            body: body.to_string(),
            html_url: String::new(),
        };
        let file =
            |name: &str, status: FileStatus, adds: u64, dels: u64, patch: Option<&str>| PullFile {
                filename: name.to_string(),
                previous_filename: None,
                status,
                additions: adds,
                deletions: dels,
                patch: patch.map(str::to_string),
            };
        let patch = "@@ -1,6 +1,9 @@\n use gpui::*;\n \n-fn open(cx: &mut App) {\n-    let bounds = Bounds::centered(None, size(px(1440.), px(920.)), cx);\n+/// Open the window where the reader left it, or centred the first time.\n+fn open(cx: &mut App, remembered: Option<Bounds<Pixels>>) {\n+    let bounds = remembered\n+        .unwrap_or_else(|| Bounds::centered(None, size(px(1440.), px(920.)), cx));\n     cx.open_window(bounds, |window, cx| shell(window, cx))\n }\n@@ -20,3 +23,4 @@ impl Shell {\n     fn persist(&mut self) {\n         self.layout.write_into(&mut self.settings);\n+        self.settings.bounds = Some(self.bounds);\n     }";
        let repo = |id: &RepoId, description: &str, private: bool, hours: i64| Repo {
            id: id.clone(),
            description: Some(description.to_string()),
            private,
            default_branch: "main".to_string(),
            stars: 12,
            open_issues: 4,
            pushed_at: Some(now - Duration::hours(hours)),
            html_url: id.html_url(),
        };

        let mut reviewer_item = item(
            &ginka,
            12,
            "Start a chat before it has a workspace",
            open,
            Status::Open,
            "alice",
            2,
            3,
            &[("enhancement", "a2eeef")],
        );
        reviewer_item.requested_reviewers.push(user("bokuweb"));
        let mut assigned = item(
            &e1,
            3,
            "Window opens behind the terminal when launched from a shell",
            Kind::Issue,
            Status::Open,
            "carol",
            5,
            1,
            &[("bug", "d73a4a"), ("macos", "0e8a16")],
        );
        assigned.assignees.push(user("bokuweb"));

        Self::empty()
            .with_viewer("bokuweb", Some("bokuweb"))
            .with_repo(repo(&e1, "A native GitHub client on GPUI", false, 1))
            .with_repo(repo(&ginka, "An IDE-agnostic coding-agent orchestrator", true, 3))
            .with_pull(pull(item(&e1, 7, "Draw the inbox with the reason where the author would be", open, Status::Open, "bokuweb", 1, 0, &[]), "inbox-rows", 212, 18, 6))
            .with_pull(pull(item(&e1, 6, "Persist the column widths across a restart", draft, Status::Open, "bokuweb", 9, 2, &[("wip", "fbca04")]), "persist-layout", 88, 12, 3))
            .with_pull(pull(item(&e1, 5, "Seed the lock file from Ginka's", merged, Status::Closed, "bokuweb", 30, 1, &[]), "seed-lock", 3, 1, 1))
            .with_pull(pull(item(&e1, 4, "Try an async client", Kind::Pull { draft: false, merged: false }, Status::Closed, "dave", 50, 4, &[]), "async", 400, 12, 9))
            .with_item(assigned)
            .with_item(item(&e1, 2, "Labels should be readable in the light theme", Kind::Issue, Status::Open, "bokuweb", 20, 0, &[("design", "c5def5")]))
            .with_item(item(&e1, 1, "Decide the licence", Kind::Issue, Status::Closed, "bokuweb", 70, 6, &[]))
            .with_pull(pull(reviewer_item, "start-a-chat-before-it-has-a-workspace", 412, 38, 9))
            .with_pull(pull(item(&ginka, 11, "Add a project, and start a chat, from the window", merged, Status::Closed, "bokuweb", 26, 2, &[]), "add-project", 300, 40, 12))
            .with_item(item(&ginka, 9, "Terminal dock forgets its height", Kind::Issue, Status::Open, "erin", 4, 2, &[("bug", "d73a4a")]))
            .with_comments(&ginka, 12, vec![
                comment(1, "bokuweb", 3, "Looks right to me. One question: does the scratch worktree get cleaned up when the chat is archived?"),
                comment(2, "alice", 2, "Not yet — I'd rather land this and do the cleanup in the archive change, since that is where the rule lives.\n\n```rust\nfn archive(&mut self) { /* … */ }\n```"),
            ])
            .with_comments(&e1, 3, vec![comment(3, "bokuweb", 4, "Reproduced. `cx.activate(true)` after the window opens fixes it.")])
            .with_files(&e1, 6, vec![
                file("src/main.rs", FileStatus::Modified, 6, 2, Some(patch)),
                file("crates/e1-ui/src/settings.rs", FileStatus::Modified, 1, 0, Some("@@ -18,4 +18,5 @@ pub struct AppSettings {\n     pub right_panel_width: f32,\n+    pub bounds: Option<WindowBounds>,\n     pub locale: Option<String>,\n     pub last_repo: Option<String>,\n }")),
                file("assets/icons/window.svg", FileStatus::Added, 0, 0, None),
            ])
            .with_tree(&e1, &[
                ("Cargo.toml", 1200), ("AGENTS.md", 6000), ("src/main.rs", 4100),
                ("crates/e1-github/src/lib.rs", 3900), ("crates/e1-github/src/rest.rs", 9000),
                ("crates/e1-ui/src/theme.rs", 12000), ("crates/e1-views/src/shell.rs", 20000),
                ("docs/roadmap.md", 15000), ("docs/ui.md", 9000), ("assets/icons/lock.svg", 300),
            ])
            .with_file(&e1, "src/main.rs", "//! The e1 desktop app.\n\nfn main() {\n    println!(\"hello from scripted data\");\n}\n")
            .with_file(&e1, "Cargo.toml", "[package]\nname = \"e1\"\nversion = \"0.0.0\"\nedition = \"2024\"\n")
            .with_tree(&ginka, &[("Cargo.toml", 2000), ("src/main.rs", 3000), ("src/shell.rs", 90000), ("docs/roadmap.md", 40000)])
            .with_file(&ginka, "src/main.rs", "//! The Ginka desktop app.\n\nfn main() {}\n")
            .with_files(&ginka, 12, vec![
                file("src/shell.rs", FileStatus::Modified, 300, 30, Some(patch)),
                file("crates/ginka-core/src/project.rs", FileStatus::Modified, 80, 8, Some("@@ -1,3 +1,4 @@\n+//! Projects, and the scratch one a chat starts in.\n use std::path::PathBuf;\n \n pub struct Project {")),
                file("docs/ui.md", FileStatus::Modified, 32, 0, Some("@@ -40,2 +40,3 @@\n ## 3. Regions\n+A chat can start before it has a workspace.\n ")),
            ])
            .with_notification(Notification {
                id: "1".into(),
                unread: true,
                reason: "review_requested".into(),
                updated_at: now - Duration::hours(2),
                repo: ginka.clone(),
                title: "Start a chat before it has a workspace".into(),
                kind: SubjectKind::PullRequest,
                number: Some(12),
            })
            .with_notification(Notification {
                id: "2".into(),
                unread: true,
                reason: "assign".into(),
                updated_at: now - Duration::hours(5),
                repo: e1.clone(),
                title: "Window opens behind the terminal when launched from a shell".into(),
                kind: SubjectKind::Issue,
                number: Some(3),
            })
            .with_notification(Notification {
                id: "3".into(),
                unread: false,
                reason: "subscribed".into(),
                updated_at: now - Duration::hours(30),
                repo: e1,
                title: "v0.1.0".into(),
                kind: SubjectKind::Release,
                number: None,
            })
    }

    fn guard(&self) -> Result<std::sync::MutexGuard<'_, Data>> {
        let data = self.data.lock().unwrap();
        match &data.failing {
            Some(message) => Err(Error::Transport(message.clone())),
            None => Ok(data),
        }
    }
}

/// One clause of a search query this app generates.
#[derive(Debug, PartialEq, Eq)]
enum Clause<'a> {
    IsPull,
    IsIssue,
    IsOpen,
    IsClosed,
    Author(&'a str),
    Assignee(&'a str),
    ReviewRequested(&'a str),
    /// A clause this evaluator does not know: it matches nothing, so a test
    /// that reaches for a new one fails visibly.
    Unknown,
}

fn parse_query(query: &str) -> Vec<Clause<'_>> {
    query
        .split_whitespace()
        .map(|word| match word.split_once(':') {
            Some(("is", "pr")) => Clause::IsPull,
            Some(("is", "issue")) => Clause::IsIssue,
            Some(("is", "open")) | Some(("state", "open")) => Clause::IsOpen,
            Some(("is", "closed")) | Some(("state", "closed")) => Clause::IsClosed,
            Some(("author", who)) => Clause::Author(who),
            Some(("assignee", who)) => Clause::Assignee(who),
            Some(("review-requested", who)) => Clause::ReviewRequested(who),
            _ => Clause::Unknown,
        })
        .collect()
}

fn matches(item: &Item, clause: &Clause<'_>, viewer: Option<&str>) -> bool {
    let me = |who: &str| -> Option<String> {
        if who == "@me" {
            viewer.map(str::to_string)
        } else {
            Some(who.to_string())
        }
    };
    match clause {
        Clause::IsPull => item.is_pull(),
        Clause::IsIssue => !item.is_pull(),
        Clause::IsOpen => item.status == Status::Open,
        Clause::IsClosed => item.status == Status::Closed,
        Clause::Author(who) => me(who).as_deref() == Some(item.author.login.as_str()),
        Clause::Assignee(who) => me(who)
            .map(|who| item.assignees.iter().any(|user| user.login == who))
            .unwrap_or(false),
        Clause::ReviewRequested(who) => me(who)
            .map(|who| {
                item.requested_reviewers
                    .iter()
                    .any(|user| user.login == who)
            })
            .unwrap_or(false),
        Clause::Unknown => false,
    }
}

impl GitHub for Scripted {
    fn viewer(&self) -> Result<Viewer> {
        self.guard()?.viewer.clone().ok_or(Error::NoToken)
    }

    fn notifications(&self) -> Result<Vec<Notification>> {
        let mut rows = self.guard()?.notifications.clone();
        rows.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(rows)
    }

    fn repositories(&self) -> Result<Vec<Repo>> {
        let mut repos = self.guard()?.repos.clone();
        repos.sort_by_key(|repo| std::cmp::Reverse(repo.pushed_at));
        Ok(repos)
    }

    fn items(&self, repo: &RepoId, kind: ListKind, status: StatusFilter) -> Result<Vec<Item>> {
        let data = self.guard()?;
        let mut rows: Vec<Item> = data
            .items
            .iter()
            .filter(|item| &item.repo == repo)
            .filter(|item| match kind {
                ListKind::Pulls => item.is_pull(),
                ListKind::Issues => !item.is_pull(),
            })
            .filter(|item| match status {
                StatusFilter::Open => item.status == Status::Open,
                StatusFilter::Closed => item.status == Status::Closed,
                StatusFilter::All => true,
            })
            .cloned()
            .collect();
        rows.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(rows)
    }

    fn search(&self, query: &str) -> Result<Vec<Item>> {
        let data = self.guard()?;
        let viewer = data.viewer.as_ref().map(|viewer| viewer.login.as_str());
        let clauses = parse_query(query);
        let mut rows: Vec<Item> = data
            .items
            .iter()
            .filter(|item| clauses.iter().all(|clause| matches(item, clause, viewer)))
            .cloned()
            .collect();
        rows.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        Ok(rows)
    }

    fn item(&self, repo: &RepoId, number: u64) -> Result<Item> {
        self.guard()?
            .items
            .iter()
            .find(|item| &item.repo == repo && item.number == number)
            .cloned()
            .ok_or_else(|| Error::Status {
                status: 404,
                path: format!("/repos/{repo}/issues/{number}"),
                message: "Not Found".into(),
            })
    }

    fn pull(&self, repo: &RepoId, number: u64) -> Result<Pull> {
        let data = self.guard()?;
        let item = data
            .items
            .iter()
            .find(|item| &item.repo == repo && item.number == number && item.is_pull())
            .cloned();
        let extra = data.pulls.get(&(repo.clone(), number)).cloned();
        match (item, extra) {
            (Some(item), Some(extra)) => Ok(Pull {
                item,
                head_sha: format!("sha-{}", extra.head),
                head: extra.head,
                base: extra.base,
                additions: extra.additions,
                deletions: extra.deletions,
                changed_files: extra.changed_files,
                mergeable: extra.mergeable,
            }),
            (Some(item), None) => Ok(Pull {
                item,
                head: "feature".into(),
                base: "main".into(),
                additions: 0,
                deletions: 0,
                changed_files: 0,
                mergeable: None,
                head_sha: String::new(),
            }),
            _ => Err(Error::Status {
                status: 404,
                path: format!("/repos/{repo}/pulls/{number}"),
                message: "Not Found".into(),
            }),
        }
    }

    fn comments(&self, repo: &RepoId, number: u64) -> Result<Vec<Comment>> {
        Ok(self
            .guard()?
            .comments
            .get(&(repo.clone(), number))
            .cloned()
            .unwrap_or_default())
    }

    fn pull_files(&self, repo: &RepoId, number: u64) -> Result<Vec<PullFile>> {
        Ok(self
            .guard()?
            .files
            .get(&(repo.clone(), number))
            .cloned()
            .unwrap_or_default())
    }

    fn comment_on(&self, repo: &RepoId, number: u64, body: &str) -> Result<Comment> {
        let mut data = self.guard()?;
        let author = data
            .viewer
            .as_ref()
            .map(|viewer| viewer.login.clone())
            .unwrap_or_else(|| "you".into());
        let comments = data.comments.entry((repo.clone(), number)).or_default();
        let comment = Comment {
            id: 1000 + comments.len() as u64,
            author: user(&author),
            created_at: Utc::now(),
            body: body.to_string(),
            html_url: String::new(),
        };
        comments.push(comment.clone());
        if let Some(item) = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number)
        {
            item.comments = Some(item.comments.unwrap_or(0) + 1);
        }
        Ok(comment)
    }

    fn set_open(&self, repo: &RepoId, number: u64, open: bool) -> Result<Item> {
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number)
            .ok_or_else(|| Error::Status {
                status: 404,
                path: format!("/repos/{repo}/issues/{number}"),
                message: "Not Found".into(),
            })?;
        item.status = if open { Status::Open } else { Status::Closed };
        item.updated_at = Utc::now();
        Ok(item.clone())
    }

    fn merge(&self, repo: &RepoId, number: u64, _method: MergeMethod) -> Result<()> {
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number && item.is_pull())
            .ok_or_else(|| Error::Status {
                status: 404,
                path: format!("/repos/{repo}/pulls/{number}"),
                message: "Not Found".into(),
            })?;
        if let Kind::Pull { draft: true, .. } = item.kind {
            return Err(Error::Status {
                status: 405,
                path: format!("/repos/{repo}/pulls/{number}/merge"),
                message: "Pull Request is still a draft".into(),
            });
        }
        item.kind = Kind::Pull {
            draft: false,
            merged: true,
        };
        item.status = Status::Closed;
        item.updated_at = Utc::now();
        Ok(())
    }

    fn checks(&self, _repo: &RepoId, sha: &str) -> Result<Checks> {
        let _guard = self.guard()?;
        // A head that says so fails; everything else passes, with one run
        // still going on a draft so the pending state has a face.
        let run = |name: &str, state: CheckState| CheckRun {
            id: if name == "build" { 1 } else { 2 },
            actions: true,
            name: name.to_string(),
            state,
            html_url: Some("https://github.com/bokuweb/e1/actions".into()),
        };
        Ok(Checks {
            runs: if sha.contains("fail") {
                vec![
                    run("build", CheckState::Success),
                    run("test", CheckState::Failure),
                ]
            } else if sha.contains("persist") {
                vec![
                    run("build", CheckState::Success),
                    run("test", CheckState::Pending),
                ]
            } else {
                vec![
                    run("build", CheckState::Success),
                    run("test", CheckState::Success),
                ]
            },
        })
    }

    fn review_comments(&self, repo: &RepoId, number: u64) -> Result<Vec<ReviewComment>> {
        let data = self.guard()?;
        Ok(data
            .review_comments
            .iter()
            .filter(|(key, _)| key == &(repo.clone(), number))
            .map(|(_, comment)| comment.clone())
            .collect())
    }

    fn review_comment(
        &self,
        repo: &RepoId,
        number: u64,
        _commit: &str,
        path: &str,
        start: Option<u32>,
        line: u32,
        side: Side,
        body: &str,
    ) -> Result<ReviewComment> {
        let mut data = self.guard()?;
        let author = data
            .viewer
            .as_ref()
            .map(|viewer| viewer.login.clone())
            .unwrap_or_else(|| "you".into());
        let comment = ReviewComment {
            id: 5000 + data.review_comments.len() as u64,
            path: path.to_string(),
            line: Some(line),
            start_line: start.filter(|start| *start < line),
            side,
            author: user(&author),
            created_at: Utc::now(),
            body: body.to_string(),
            html_url: String::new(),
        };
        data.review_comments
            .push(((repo.clone(), number), comment.clone()));
        Ok(comment)
    }

    fn pull_checks(&self, keys: &[(RepoId, u64)]) -> Result<HashMap<(RepoId, u64), CheckState>> {
        let heads: Vec<(RepoId, u64, String)> = {
            let data = self.guard()?;
            keys.iter()
                .filter_map(|key| {
                    data.pulls
                        .get(key)
                        .map(|pull| (key.0.clone(), key.1, pull.head.clone()))
                })
                .collect()
        };
        heads
            .into_iter()
            .map(|(repo, number, head)| {
                self.checks(&repo, &head)
                    .map(|checks| ((repo, number), checks.overall()))
            })
            .collect()
    }

    fn set_draft(&self, node_id: &str, draft: bool) -> Result<()> {
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| item.node_id == node_id)
            .ok_or(Error::Unsupported("change a missing pull"))?;
        if let Kind::Pull { merged, .. } = item.kind {
            item.kind = Kind::Pull { draft, merged };
        }
        Ok(())
    }

    fn commits(&self, repo: &RepoId, page: u32) -> Result<Vec<Commit>> {
        let _guard = self.guard()?;
        // Two pages of scripted history, so paging can be seen to work.
        if page > 2 {
            return Ok(Vec::new());
        }
        // Full hashes, and parents that name them: the rail is laid out by
        // matching a parent to a commit, so a short hash here would draw a
        // history where nothing joins up.
        const TIP: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";
        const MERGE: &str = "b2c3d4e5f60718293a4b5c6d7e8f90123456789a";
        const TRUNK: &str = "c3d4e5f60718293a4b5c6d7e8f90123456789ab2";
        const SIDE: &str = "d4e5f60718293a4b5c6d7e8f90123456789ab2c3";
        const BASE: &str = "e5f60718293a4b5c6d7e8f90123456789ab2c3d4";
        const ROOT: &str = "f60718293a4b5c6d7e8f90123456789ab2c3d4e5";
        let at = |days: i64| Utc::now() - chrono::Duration::days(days);
        let commit = |sha: &str, message: &str, days: i64, parents: &[&str]| Commit {
            sha: sha.to_string(),
            message: message.to_string(),
            author_name: "bokuweb".into(),
            author: Some(user("bokuweb")),
            authored_at: at(days),
            parents: parents.iter().map(|parent| parent.to_string()).collect(),
            html_url: format!("https://github.com/{repo}/commit/{sha}"),
        };
        if page == 2 {
            // The second page: what scrolling back reaches.
            return Ok((1..=4)
                .map(|n| {
                    commit(
                        &format!("{n:040x}"),
                        &format!("An older commit, number {n}"),
                        20 + n as i64,
                        &[],
                    )
                })
                .collect());
        }
        // The first page runs past a window's worth, so that reaching the
        // end of it is something a reader has to scroll to do.
        let filler: Vec<Commit> = (1..=24)
            .map(|n| {
                commit(
                    &format!("{:040x}", 100 + n),
                    &format!("A commit from further back, number {n}"),
                    5 + n as i64,
                    &[],
                )
            })
            .collect();
        let mut page_one = vec![
            commit(
                TIP,
                "Group a job's log by its steps\n\nThe steps come from the Actions API.",
                0,
                &[MERGE],
            ),
            commit(
                MERGE,
                "Merge pull request #12 from bokuweb/files-tree",
                1,
                &[TRUNK, SIDE],
            ),
            commit(TRUNK, "Poll what is still running", 1, &[BASE]),
            commit(SIDE, "Build the files column as a tree", 2, &[BASE]),
            commit(BASE, "Rebuild the light theme", 3, &[ROOT]),
            commit(ROOT, "Start a chat before it has a workspace", 5, &[]),
        ];
        page_one.extend(filler);
        Ok(page_one)
    }

    fn commit(&self, repo: &RepoId, sha: &str) -> Result<CommitDetail> {
        let commit = self
            .commits(repo, 1)?
            .into_iter()
            .find(|commit| commit.sha.starts_with(sha) || sha.starts_with(commit.short()))
            .ok_or(Error::Unsupported("read a commit that is not scripted"))?;
        let files = vec![PullFile {
            filename: "src/shell.rs".into(),
            previous_filename: None,
            status: FileStatus::Modified,
            additions: 3,
            deletions: 1,
            patch: Some(
                "@@ -1,4 +1,6 @@\n use gpui::*;\n-fn open(cx: &mut App) {\n+/// Open the window.\n+fn open(cx: &mut App, remembered: Option<Bounds<Pixels>>) {\n+    let bounds = remembered;\n     cx.open_window(bounds, |window, cx| shell(window, cx))\n }"
                    .into(),
            ),
        }];
        Ok(CommitDetail {
            commit,
            additions: 3,
            deletions: 1,
            files,
        })
    }

    fn job(&self, _repo: &RepoId, job_id: u64) -> Result<Job> {
        let _guard = self.guard()?;
        let at = |second: u32| {
            Some(
                chrono::DateTime::parse_from_rfc3339(&format!("2026-09-07T00:00:{second:02}Z"))
                    .unwrap()
                    .with_timezone(&Utc),
            )
        };
        let step = |number: u64, name: &str, state: CheckState, from: u32, to: u32| JobStep {
            number,
            name: name.to_string(),
            state,
            started_at: at(from),
            completed_at: at(to),
        };
        // Job 2 is the one whose test failed in the sample checks.
        let failing = job_id == 2;
        Ok(Job {
            id: job_id,
            run_id: 900 + job_id,
            name: if job_id == 1 { "build" } else { "test" }.to_string(),
            state: if failing {
                CheckState::Failure
            } else {
                CheckState::Success
            },
            steps: vec![
                step(1, "Set up job", CheckState::Success, 0, 3),
                step(2, "Run actions/checkout@v4", CheckState::Success, 3, 8),
                step(3, "Install dependencies", CheckState::Success, 8, 20),
                step(
                    4,
                    "Run cargo test",
                    if failing {
                        CheckState::Failure
                    } else {
                        CheckState::Success
                    },
                    20,
                    36,
                ),
                step(5, "Upload artifacts", CheckState::Neutral, 36, 36),
                step(6, "Complete job", CheckState::Success, 36, 40),
            ],
            html_url: "https://github.com/bokuweb/e1/actions".into(),
        })
    }

    fn job_log(&self, _repo: &RepoId, job_id: u64) -> Result<String> {
        let _guard = self.guard()?;
        let mut lines: Vec<String> = Vec::new();
        for n in 0..40u32 {
            let stamp = format!("2026-09-07T00:00:{n:02}.100Z");
            let text = match n {
                0 => "##[group]Runner Image".to_string(),
                1 => "Image: ubuntu-24.04".to_string(),
                2 => "##[endgroup]".to_string(),
                3 => "##[group]Run actions/checkout@v4".to_string(),
                8 => "##[command]cargo fetch".to_string(),
                20 => "##[command]cargo test --workspace".to_string(),
                30 if job_id == 2 => "##[error]test result: FAILED. 1 failed".to_string(),
                _ => format!("job {job_id}: line {n} of 40 … ok"),
            };
            lines.push(format!("{stamp} {text}"));
        }
        Ok(lines.join("\n"))
    }

    fn review(&self, repo: &RepoId, number: u64, event: ReviewEvent, body: &str) -> Result<()> {
        // A review is a comment with a verdict on the front; the fake keeps
        // the verdict as words, which is what the timeline would show.
        let text = format!("[{}] {body}", event.as_api());
        self.comment_on(repo, number, &text).map(|_| ())
    }

    fn labels(&self, _repo: &RepoId) -> Result<Vec<Label>> {
        let _guard = self.guard()?;
        Ok([
            ("bug", "d73a4a", "Something isn't working"),
            ("enhancement", "a2eeef", "New feature or request"),
            ("design", "c5def5", "How it looks and reads"),
            ("wip", "fbca04", "Not ready for review"),
            ("macos", "0e8a16", "Only on macOS"),
        ]
        .iter()
        .map(|(name, color, description)| Label {
            name: name.to_string(),
            color: color.to_string(),
            description: Some(description.to_string()),
        })
        .collect())
    }

    fn add_labels(&self, repo: &RepoId, number: u64, labels: &[String]) -> Result<Item> {
        let offered = self.labels(repo)?;
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number)
            .ok_or(Error::Unsupported("label a missing item"))?;
        for name in labels {
            if !item.labels.iter().any(|label| &label.name == name) {
                let color = offered
                    .iter()
                    .find(|label| &label.name == name)
                    .map(|label| label.color.clone())
                    .unwrap_or_else(|| "888888".into());
                let description = offered
                    .iter()
                    .find(|label| &label.name == name)
                    .and_then(|label| label.description.clone());
                item.labels.push(Label {
                    name: name.clone(),
                    color,
                    description,
                });
            }
        }
        Ok(item.clone())
    }

    fn remove_label(&self, repo: &RepoId, number: u64, label: &str) -> Result<Item> {
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number)
            .ok_or(Error::Unsupported("unlabel a missing item"))?;
        item.labels.retain(|existing| existing.name != label);
        Ok(item.clone())
    }

    fn assignees(&self, _repo: &RepoId) -> Result<Vec<User>> {
        let _guard = self.guard()?;
        Ok(["bokuweb", "alice", "carol", "dave"]
            .iter()
            .map(|login| user(login))
            .collect())
    }

    fn add_assignees(&self, repo: &RepoId, number: u64, logins: &[String]) -> Result<Item> {
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number)
            .ok_or(Error::Unsupported("assign a missing item"))?;
        for login in logins {
            if !item.assignees.iter().any(|user| &user.login == login) {
                item.assignees.push(user(login));
            }
        }
        Ok(item.clone())
    }

    fn remove_assignees(&self, repo: &RepoId, number: u64, logins: &[String]) -> Result<Item> {
        let mut data = self.guard()?;
        let item = data
            .items
            .iter_mut()
            .find(|item| &item.repo == repo && item.number == number)
            .ok_or(Error::Unsupported("unassign a missing item"))?;
        item.assignees.retain(|user| !logins.contains(&user.login));
        Ok(item.clone())
    }

    fn projects(&self, owner: &str) -> Result<Vec<Project>> {
        let _guard = self.guard()?;
        Ok(vec![
            Project {
                id: format!("PVT_{owner}_1"),
                owner: owner.to_string(),
                title: "Roadmap".into(),
                number: 1,
                closed: false,
                html_url: format!("https://github.com/users/{owner}/projects/1"),
            },
            Project {
                id: format!("PVT_{owner}_2"),
                owner: owner.to_string(),
                title: "Bugs".into(),
                number: 2,
                closed: false,
                html_url: format!("https://github.com/users/{owner}/projects/2"),
            },
        ])
    }

    fn all_projects(&self) -> Result<Vec<Project>> {
        self.projects("bokuweb")
    }

    fn project(&self, project: &Project) -> Result<ProjectBoard> {
        let data = self.guard()?;
        let items = data
            .items
            .iter()
            .take(12)
            .map(|item| ProjectItem {
                id: format!("PVTI_{}_{}", item.repo.name, item.number),
                title: item.title.clone(),
                kind: if item.is_pull() {
                    ProjectItemKind::PullRequest
                } else {
                    ProjectItemKind::Issue
                },
                repo: Some(item.repo.clone()),
                number: Some(item.number),
                state: Some(format!("{:?}", item.status).to_uppercase()),
                status: Some(if item.status == Status::Closed {
                    "Done".into()
                } else {
                    "In progress".into()
                }),
                fields: vec![
                    ProjectFieldValue {
                        field_id: "status".into(),
                        field_name: "Status".into(),
                        value: ProjectValue::SingleSelect(if item.status == Status::Closed {
                            "Done".into()
                        } else {
                            "In progress".into()
                        }),
                    },
                    ProjectFieldValue {
                        field_id: "iteration".into(),
                        field_name: "Iteration".into(),
                        value: ProjectValue::Iteration {
                            title: "September".into(),
                            start_date: "2026-09-01".into(),
                            duration: 30,
                        },
                    },
                ],
                html_url: Some(item.html_url.clone()),
                archived: false,
            })
            .collect();
        Ok(ProjectBoard {
            project: project.clone(),
            items,
            fields: vec![
                ProjectField {
                    id: "status".into(),
                    name: "Status".into(),
                    data_type: "SINGLE_SELECT".into(),
                    options: ["Todo", "In progress", "Done"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| ProjectFieldOption {
                            id: format!("status-{index}"),
                            name: name.into(),
                            color: "GRAY".into(),
                        })
                        .collect(),
                },
                ProjectField {
                    id: "iteration".into(),
                    name: "Iteration".into(),
                    data_type: "ITERATION".into(),
                    options: Vec::new(),
                },
            ],
            views: vec![
                ProjectView {
                    id: "table".into(),
                    name: "Table".into(),
                    number: 1,
                    layout: ProjectViewLayout::Table,
                    filter: None,
                    group_by: None,
                    vertical_group_by: None,
                    visible_fields: vec!["status".into()],
                },
                ProjectView {
                    id: "board".into(),
                    name: "Kanban".into(),
                    number: 2,
                    layout: ProjectViewLayout::Board,
                    filter: None,
                    group_by: None,
                    vertical_group_by: Some("status".into()),
                    visible_fields: vec!["status".into()],
                },
                ProjectView {
                    id: "roadmap".into(),
                    name: "Roadmap".into(),
                    number: 3,
                    layout: ProjectViewLayout::Roadmap,
                    filter: None,
                    group_by: None,
                    vertical_group_by: None,
                    visible_fields: Vec::new(),
                },
            ],
        })
    }

    fn set_project_single_select(
        &self,
        _project_id: &str,
        _item_id: &str,
        _field_id: &str,
        _option_id: Option<&str>,
    ) -> Result<()> {
        let _guard = self.guard()?;
        Ok(())
    }

    fn item_projects(&self, repo: &RepoId, number: u64) -> Result<Vec<ProjectMembership>> {
        let data = self.guard()?;
        Ok(data
            .memberships
            .iter()
            .filter(|(key, _)| key == &(repo.clone(), number))
            .map(|(_, membership)| membership.clone())
            .collect())
    }

    fn add_to_project(&self, project_id: &str, node_id: &str) -> Result<()> {
        let mut data = self.guard()?;
        let key = data
            .items
            .iter()
            .find(|item| item.node_id == node_id)
            .map(|item| (item.repo.clone(), item.number))
            .ok_or(Error::Unsupported("add a missing item to a project"))?;
        let title = if project_id.ends_with("_1") {
            "Roadmap"
        } else {
            "Bugs"
        };
        data.memberships.push((
            key,
            ProjectMembership {
                project_id: project_id.to_string(),
                title: title.to_string(),
                item_id: format!("PVTI_{project_id}_{node_id}"),
            },
        ));
        Ok(())
    }

    fn remove_from_project(&self, _project_id: &str, item_id: &str) -> Result<()> {
        let mut data = self.guard()?;
        data.memberships
            .retain(|(_, membership)| membership.item_id != item_id);
        Ok(())
    }

    fn tree(&self, repo: &RepoId) -> Result<Tree> {
        Ok(self.guard()?.trees.get(repo).cloned().unwrap_or(Tree {
            entries: Vec::new(),
            truncated: false,
        }))
    }

    fn file(&self, repo: &RepoId, path: &str) -> Result<FileContent> {
        let data = self.guard()?;
        let text = data
            .contents
            .get(&(repo.clone(), path.to_string()))
            .cloned();
        let size = data
            .trees
            .get(repo)
            .and_then(|tree| tree.entries.iter().find(|entry| entry.path == path))
            .and_then(|entry| entry.size)
            .unwrap_or(0);
        Ok(FileContent {
            path: path.to_string(),
            size,
            text,
            html_url: format!("{}/blob/main/{path}", repo.html_url()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_answers_every_question_the_window_asks() {
        let github = Scripted::sample();
        let viewer = github.viewer().unwrap();
        assert_eq!(viewer.login, "bokuweb");
        assert_eq!(github.repositories().unwrap().len(), 2);
        assert!(github.notifications().unwrap().iter().any(|n| n.unread));

        let e1 = RepoId::new("bokuweb", "e1");
        let open_pulls = github
            .items(&e1, ListKind::Pulls, StatusFilter::Open)
            .unwrap();
        assert!(
            open_pulls
                .iter()
                .all(|item| item.is_pull() && item.status == Status::Open)
        );
        let closed = github
            .items(&e1, ListKind::Pulls, StatusFilter::Closed)
            .unwrap();
        assert!(closed.iter().any(|item| item.state() == State::Merged));
        let issues = github
            .items(&e1, ListKind::Issues, StatusFilter::All)
            .unwrap();
        assert!(issues.iter().all(|item| !item.is_pull()));

        let pull = github.pull(&RepoId::new("bokuweb", "ginka"), 12).unwrap();
        assert_eq!(pull.changed_files, 9);
        assert_eq!(github.comments(&pull.item.repo, 12).unwrap().len(), 2);
        let files = github.pull_files(&pull.item.repo, 12).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files[0].patch.is_some());

        let tree = github.tree(&e1).unwrap();
        assert!(
            tree.entries
                .iter()
                .any(|e| e.path == "crates" && e.kind == EntryKind::Tree)
        );
        assert!(tree.files().any(|e| e.path == "src/main.rs"));
        let file = github.file(&e1, "src/main.rs").unwrap();
        assert!(file.text.unwrap().contains("fn main"));
        assert!(
            github
                .file(&e1, "assets/icons/lock.svg")
                .unwrap()
                .text
                .is_none()
        );
    }

    #[test]
    fn the_search_evaluator_understands_the_queries_this_app_generates() {
        let github = Scripted::sample();
        let mine = github.search("is:pr author:@me is:open").unwrap();
        assert!(!mine.is_empty());
        assert!(
            mine.iter()
                .all(|item| item.author.login == "bokuweb" && item.is_pull())
        );

        let reviews = github.search("is:pr review-requested:@me is:open").unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].number, 12);

        let assigned = github.search("is:issue assignee:@me is:open").unwrap();
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].number, 3);
    }

    #[test]
    fn writing_changes_what_the_next_read_says() {
        let github = Scripted::sample();
        let e1 = RepoId::new("bokuweb", "e1");
        let before = github.comments(&e1, 2).unwrap().len();
        let comment = github.comment_on(&e1, 2, "On it.").unwrap();
        assert_eq!(comment.author.login, "bokuweb", "the viewer wrote it");
        assert_eq!(github.comments(&e1, 2).unwrap().len(), before + 1);
        assert_eq!(github.item(&e1, 2).unwrap().comments, Some(1));

        let closed = github.set_open(&e1, 2, false).unwrap();
        assert_eq!(closed.state(), State::Closed);
        assert_eq!(github.set_open(&e1, 2, true).unwrap().state(), State::Open);

        // A draft cannot be merged; a ready pull can, and is then merged.
        assert!(github.merge(&e1, 6, MergeMethod::Squash).is_err());
        github.merge(&e1, 7, MergeMethod::Squash).unwrap();
        assert_eq!(github.item(&e1, 7).unwrap().state(), State::Merged);
        assert!(matches!(
            Scripted::empty().avatar("x"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn labels_assignees_reviews_and_projects_round_trip() {
        let github = Scripted::sample();
        let e1 = RepoId::new("bokuweb", "e1");
        assert!(github.labels(&e1).unwrap().iter().any(|l| l.name == "bug"));
        let item = github
            .add_labels(&e1, 2, &["bug".into(), "wip".into()])
            .unwrap();
        assert_eq!(item.labels.len(), 3, "design was there already");
        let item = github.remove_label(&e1, 2, "design").unwrap();
        assert!(item.labels.iter().all(|l| l.name != "design"));

        let item = github.add_assignees(&e1, 2, &["alice".into()]).unwrap();
        assert_eq!(item.assignees.len(), 1);
        let item = github.remove_assignees(&e1, 2, &["alice".into()]).unwrap();
        assert!(item.assignees.is_empty());

        github.review(&e1, 7, ReviewEvent::Approve, "LGTM").unwrap();
        let last = github.comments(&e1, 7).unwrap().pop().unwrap();
        assert!(last.body.starts_with("[APPROVE]"));

        let projects = github.projects("bokuweb").unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(github.all_projects().unwrap(), projects);
        let board = github.project(&projects[0]).unwrap();
        assert_eq!(board.project, projects[0]);
        assert!(!board.items.is_empty());
        assert!(board.items.iter().all(|item| item.item_key().is_some()));
        let node = github.item(&e1, 2).unwrap().node_id;
        github.add_to_project(&projects[0].id, &node).unwrap();
        let memberships = github.item_projects(&e1, 2).unwrap();
        assert_eq!(memberships.len(), 1);
        github
            .remove_from_project(&projects[0].id, &memberships[0].item_id)
            .unwrap();
        assert!(github.item_projects(&e1, 2).unwrap().is_empty());
    }

    #[test]
    fn line_comments_drafts_and_logs_round_trip() {
        let github = Scripted::sample();
        let e1 = RepoId::new("bokuweb", "e1");
        assert!(github.review_comments(&e1, 7).unwrap().is_empty());
        let comment = github
            .review_comment(
                &e1,
                7,
                "sha",
                "src/main.rs",
                None,
                4,
                Side::Right,
                "Why here?",
            )
            .unwrap();
        assert_eq!(comment.line, Some(4));
        assert_eq!(github.review_comments(&e1, 7).unwrap().len(), 1);

        let node = github.item(&e1, 6).unwrap().node_id;
        assert_eq!(github.item(&e1, 6).unwrap().state(), State::Draft);
        github.set_draft(&node, false).unwrap();
        assert_eq!(github.item(&e1, 6).unwrap().state(), State::Open);
        github.set_draft(&node, true).unwrap();
        assert_eq!(github.item(&e1, 6).unwrap().state(), State::Draft);

        let log = github.job_log(&e1, 1).unwrap();
        assert_eq!(log.lines().count(), 40);
        let ginka = RepoId::new("bokuweb", "ginka");
        let statuses = github
            .pull_checks(&[(e1.clone(), 7), (ginka.clone(), 12), (e1.clone(), 999)])
            .unwrap();
        assert!(statuses.contains_key(&(ginka.clone(), 12)));
        assert!(!statuses.contains_key(&(e1.clone(), 999)));
        let ranged = github
            .review_comment(
                &e1,
                7,
                "sha",
                "src/main.rs",
                Some(2),
                4,
                Side::Right,
                "these",
            )
            .unwrap();
        assert_eq!(ranged.start_line, Some(2));
        let history = github.commits(&ginka, 1).unwrap();
        assert_eq!(history.len(), 30, "a page long enough to scroll");
        assert_eq!(github.commits(&ginka, 2).unwrap().len(), 4, "an older page");
        assert!(
            github.commits(&ginka, 3).unwrap().is_empty(),
            "a short page is the last one"
        );
        assert!(
            history[1].is_merge(),
            "the second commit brings two lines together"
        );
        assert_eq!(history[0].subject(), "Group a job's log by its steps");
        assert_eq!(history[0].body(), "The steps come from the Actions API.");
        assert_eq!(history[0].short().len(), 7);
        let one = github.commit(&ginka, history[0].short()).unwrap();
        assert_eq!(one.commit.sha, history[0].sha);
        assert_eq!(one.files.len(), 1);
        let job = github.job(&e1, 2).unwrap();
        assert_eq!(job.steps.len(), 6);
        assert_eq!(job.steps[3].state, CheckState::Failure);
        assert_eq!(job.steps[2].duration(), "12s");
        let checks = github.checks(&e1, "sha-inbox-rows").unwrap();
        assert!(checks.runs.iter().all(|run| run.actions && run.id > 0));
    }

    #[test]
    fn an_unknown_clause_matches_nothing_rather_than_everything() {
        let github = Scripted::sample();
        assert!(github.search("is:pr label:bug").unwrap().is_empty());
    }

    #[test]
    fn a_failing_source_fails_every_call() {
        let github = Scripted::sample().failing("offline");
        assert!(matches!(github.viewer(), Err(Error::Transport(_))));
        assert!(github.repositories().is_err());
    }

    #[test]
    fn an_empty_source_has_no_viewer() {
        assert!(matches!(Scripted::empty().viewer(), Err(Error::NoToken)));
    }
}
