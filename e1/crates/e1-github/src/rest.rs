//! The [`GitHub`] trait over GitHub's REST API.
//!
//! Blocking `ureq` over rustls, run by the caller on a background thread: no
//! async runtime, which is what lets this crate be linked into a host that has
//! its own (`docs/roadmap.md` E3). Listings follow the `Link: rel="next"`
//! header up to a page cap, because an unbounded walk of a large
//! repository's closed issues is a hang, not a feature. With an
//! [`HttpCache`] attached every answer is kept with its `ETag`, and a
//! refresh that GitHub answers with `304` is served from disk without
//! spending the rate limit.

use crate::auth::Token;
use crate::cache::{Cached, HttpCache};
use crate::model::*;
use crate::wire::*;
use crate::{Error, GitHub, ListKind, Result, StatusFilter};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::time::Duration;

/// Where GitHub's API is.
pub const API: &str = "https://api.github.com";

/// How many rows a page of a listing holds. A hundred is GitHub's ceiling
/// for every endpoint here, and asking for fewer only means asking again.
///
/// Public because a caller that pages for itself — the history, walked back
/// as it is scrolled — has to know that a shorter page is the last one.
pub const PAGE_SIZE: usize = 100;

/// How many rows GitHub will return on one notifications page.
///
/// Unlike the other listings, `/notifications` silently caps this at fifty.
const NOTIFICATION_PAGE_SIZE: usize = 50;

/// How many pages a listing walks before stopping.
///
/// Following the `next` link normally ends the walk. This high ceiling is a
/// guard against a broken or circular link, not a product limit: it allows up
/// to 5,000 notifications or 10,000 rows from the other listings.
const PAGE_CAP: usize = 100;

/// A Project opens with only what can fill the visible board. Later requests
/// return to GitHub's maximum page size once there is content to interact with.
const PROJECT_FIRST_PAGE_SIZE: usize = 25;

/// How long one request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// GitHub over HTTPS.
pub struct Rest {
    agent: ureq::Agent,
    token: Token,
    base: String,
    cache: Option<HttpCache>,
}

impl Rest {
    /// A client for `api.github.com` with this token.
    pub fn new(token: Token) -> Self {
        Self::with_base(token, API)
    }

    /// A client for another host, which is how a test or an enterprise
    /// instance would point it elsewhere.
    pub fn with_base(token: Token, base: impl Into<String>) -> Self {
        let config = ureq::Agent::config_builder()
            // Non-2xx answers are read for their `message` rather than thrown
            // away as an error with no body.
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build();
        Self {
            agent: config.into(),
            token,
            base: base.into().trim_end_matches('/').to_string(),
            cache: None,
        }
    }

    /// Keep answers in this cache and revalidate them with `If-None-Match`.
    pub fn with_cache(mut self, cache: HttpCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// One GET, decoded. `path` is absolute (`/user`) or, for a `Link`
    /// continuation, a full URL.
    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<(T, Option<String>)> {
        let (body, next) = self.fetch(path)?;
        let value =
            serde_json::from_str(&body).map_err(|error| Error::Decode(error.to_string()))?;
        Ok((value, next))
    }

    /// One GET, as text, through the cache when there is one.
    fn fetch(&self, path: &str) -> Result<(String, Option<String>)> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{path}", self.base)
        };
        let cached = self.cache.as_ref().and_then(|cache| cache.load(&url));
        let mut request = self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token.secret()))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "e1");
        if let Some(cached) = &cached {
            request = request.header("If-None-Match", &cached.etag);
        }
        let mut response = request
            .call()
            .map_err(|error| Error::Transport(error.to_string()))?;

        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let next = header("link").and_then(|link| next_link(&link));
        let remaining = header("x-ratelimit-remaining");
        let reset = header("x-ratelimit-reset");
        let etag = header("etag");

        if status == 304
            && let Some(cached) = cached
        {
            // Unchanged since the tag: the kept body is the answer, and the
            // kept `next` is the page after it, which a `304` does not say.
            return Ok((cached.body, cached.next));
        }

        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| Error::Transport(error.to_string()))?;

        if (200..300).contains(&status) {
            if let (Some(cache), Some(etag)) = (&self.cache, etag) {
                let kept = Cached {
                    etag,
                    body: body.clone(),
                    next: next.clone(),
                };
                if let Err(error) = cache.store(&url, &kept) {
                    tracing::debug!(%error, "could not keep the answer");
                }
            }
            return Ok((body, next));
        }
        if matches!(status, 403 | 429) && remaining.as_deref() == Some("0") {
            return Err(Error::RateLimited {
                reset: reset
                    .and_then(|epoch| epoch.parse::<i64>().ok())
                    .and_then(reset_at),
            });
        }
        let message = serde_json::from_str::<WireMessage>(&body)
            .map(|wire| wire.message)
            .unwrap_or_default();
        Err(Error::Status {
            status,
            path: path.to_string(),
            message,
        })
    }

    /// One request with a JSON body, decoded. Nothing here goes through the
    /// cache: a write's answer is the new state, and GitHub does not tag it.
    fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base);
        let request = match method {
            "POST" => self.agent.post(&url),
            "PATCH" => self.agent.patch(&url),
            "DELETE" => self.agent.delete(&url).force_send_body(),
            _ => self.agent.put(&url),
        };
        let mut response = request
            .header("Authorization", &format!("Bearer {}", self.token.secret()))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "e1")
            .send_json(body)
            .map_err(|error| Error::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| Error::Transport(error.to_string()))?;
        if (200..300).contains(&status) {
            return serde_json::from_str(&text).map_err(|error| Error::Decode(error.to_string()));
        }
        let message = serde_json::from_str::<WireMessage>(&text)
            .map(|wire| wire.message)
            .unwrap_or_default();
        Err(Error::Status {
            status,
            path: path.to_string(),
            message,
        })
    }

    /// One GraphQL request. Projects are only reachable this way; everything
    /// else stays on REST, where the `ETag` cache works.
    ///
    /// GraphQL answers `200` to a refused query and puts the refusal in
    /// `errors`, so that is checked here rather than by status.
    fn graphql(&self, query: &str, variables: serde_json::Value) -> Result<serde_json::Value> {
        let answer: serde_json::Value = self.send(
            "POST",
            "/graphql",
            serde_json::json!({ "query": query, "variables": variables }),
        )?;
        if let Some(errors) = answer["errors"].as_array()
            && let Some(first) = errors.first()
        {
            let message = first["message"]
                .as_str()
                .unwrap_or("GraphQL refused the query");
            return Err(Error::Status {
                status: 200,
                path: "/graphql".into(),
                message: message.to_string(),
            });
        }
        Ok(answer["data"].clone())
    }

    /// Every page of a listing, up to [`PAGE_CAP`].
    fn get_pages<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        walk_pages(path, |url| self.get(url))
    }
}

/// Follow a listing's `next` links while keeping one ceiling for a broken
/// chain. The request itself is injected so the walk is testable without a
/// network or a GitHub token.
fn walk_pages<T, E>(
    path: &str,
    mut get: impl FnMut(&str) -> std::result::Result<(Vec<T>, Option<String>), E>,
) -> std::result::Result<Vec<T>, E> {
    let mut collected = Vec::new();
    let mut next = Some(path.to_string());
    let mut pages = 0;
    while let Some(url) = next.take() {
        if pages == PAGE_CAP {
            break;
        }
        pages += 1;
        let (page, following) = get(&url)?;
        collected.extend(page);
        next = following;
    }
    Ok(collected)
}

/// The `next` URL out of a `Link` header, if it has one.
///
/// The header is a comma-separated list of `<url>; rel="name"` entries. Only
/// `next` is read: `last` would say how far the walk could go, and the cap
/// decides that instead.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|entry| {
        let (url, params) = entry.split_once(';')?;
        if !params.contains("rel=\"next\"") {
            return None;
        }
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        Some(url.to_string())
    })
}

/// A Unix timestamp as GitHub's `x-ratelimit-reset` sends it.
fn reset_at(epoch: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(epoch, 0)
}

/// Percent-encode a search query for the URL.
///
/// GitHub's search syntax is spaces and colons; only the characters that
/// would end or split the query need escaping, so a hand-rolled encoder is
/// smaller than a dependency and no less correct for this input.
fn encode_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// GraphQL's `StatusState` as ours.
fn rollup_state(state: &str) -> CheckState {
    match state {
        "SUCCESS" => CheckState::Success,
        "FAILURE" | "ERROR" => CheckState::Failure,
        "PENDING" | "EXPECTED" => CheckState::Pending,
        _ => CheckState::Neutral,
    }
}

impl GitHub for Rest {
    fn viewer(&self) -> Result<Viewer> {
        let (user, _): (WireUser, _) = self.get("/user")?;
        Ok(user.into())
    }

    fn notifications(&self) -> Result<Vec<Notification>> {
        let pages: Vec<WireNotification> =
            self.get_pages(&format!("/notifications?per_page={NOTIFICATION_PAGE_SIZE}"))?;
        Ok(pages
            .into_iter()
            .filter_map(WireNotification::into_notification)
            .collect())
    }

    fn repositories(&self) -> Result<Vec<Repo>> {
        let pages: Vec<WireRepo> = self.get_pages(
            &format!(
                "/user/repos?sort=pushed&per_page={PAGE_SIZE}&affiliation=owner,collaborator,organization_member"
            ),
        )?;
        Ok(pages.into_iter().filter_map(WireRepo::into_repo).collect())
    }

    fn items(&self, repo: &RepoId, kind: ListKind, status: StatusFilter) -> Result<Vec<Item>> {
        let state = status.as_query();
        match kind {
            ListKind::Pulls => {
                let path = format!(
                    "/repos/{repo}/pulls?state={state}&per_page={PAGE_SIZE}&sort=updated&direction=desc"
                );
                let pages: Vec<WirePull> = self.get_pages(&path)?;
                Ok(pages.into_iter().map(|pull| pull.into_item(repo)).collect())
            }
            ListKind::Issues => {
                let path = format!(
                    "/repos/{repo}/issues?state={state}&per_page={PAGE_SIZE}&sort=updated&direction=desc"
                );
                let pages: Vec<WireIssue> = self.get_pages(&path)?;
                Ok(pages
                    .into_iter()
                    .filter(|issue| !issue.is_pull())
                    .filter_map(|issue| issue.into_item(Some(repo)))
                    .collect())
            }
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Item>> {
        // Search answers with an object where the listings answer with an
        // array, so this cannot go through `get_pages`; the walk is the
        // same. Without it the three search-backed sections stopped at one
        // page while every other list walked to the cap.
        let mut next = Some(format!(
            "/search/issues?q={}&per_page={PAGE_SIZE}&sort=updated&order=desc",
            encode_query(query)
        ));
        let mut items = Vec::new();
        let mut pages = 0;
        while let Some(url) = next.take() {
            if pages == PAGE_CAP {
                break;
            }
            pages += 1;
            let (search, following): (WireSearch, _) = self.get(&url)?;
            items.extend(
                search
                    .items
                    .into_iter()
                    .filter_map(|issue| issue.into_item(None)),
            );
            next = following;
        }
        Ok(items)
    }

    fn item(&self, repo: &RepoId, number: u64) -> Result<Item> {
        let (issue, _): (WireIssue, _) = self.get(&format!("/repos/{repo}/issues/{number}"))?;
        if issue.is_pull() {
            // The issue view of a pull does not say whether it is a draft or
            // what it merges into; the pull endpoint does.
            return self.pull(repo, number).map(|pull| pull.item);
        }
        issue
            .into_item(Some(repo))
            .ok_or_else(|| Error::Decode("item without a repository".into()))
    }

    fn pull(&self, repo: &RepoId, number: u64) -> Result<Pull> {
        let (pull, _): (WirePull, _) = self.get(&format!("/repos/{repo}/pulls/{number}"))?;
        Ok(pull.into_pull(repo))
    }

    fn comments(&self, repo: &RepoId, number: u64) -> Result<Vec<Comment>> {
        let path = format!("/repos/{repo}/issues/{number}/comments?per_page={PAGE_SIZE}");
        let pages: Vec<WireComment> = self.get_pages(&path)?;
        Ok(pages.into_iter().map(Into::into).collect())
    }

    fn pull_files(&self, repo: &RepoId, number: u64) -> Result<Vec<PullFile>> {
        let path = format!("/repos/{repo}/pulls/{number}/files?per_page={PAGE_SIZE}");
        let pages: Vec<WirePullFile> = self.get_pages(&path)?;
        Ok(pages.into_iter().map(Into::into).collect())
    }

    fn tree(&self, repo: &RepoId) -> Result<Tree> {
        // `HEAD` is the default branch without a round trip to ask which.
        let (tree, _): (WireTree, _) =
            self.get(&format!("/repos/{repo}/git/trees/HEAD?recursive=1"))?;
        Ok(tree.into())
    }

    fn avatar(&self, url: &str) -> Result<Vec<u8>> {
        // GitHub's avatar host takes `s` for the size; a row needs 80 px at
        // most, and the default is 460.
        let sized = if url.contains('?') {
            format!("{url}&s=80")
        } else {
            format!("{url}?s=80")
        };
        let mut response = self
            .agent
            .get(&sized)
            .header("User-Agent", "e1")
            .call()
            .map_err(|error| Error::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Error::Status {
                status,
                path: url.to_string(),
                message: String::new(),
            });
        }
        response
            .body_mut()
            .read_to_vec()
            .map_err(|error| Error::Transport(error.to_string()))
    }

    fn comment_on(&self, repo: &RepoId, number: u64, body: &str) -> Result<Comment> {
        let comment: WireComment = self.send(
            "POST",
            &format!("/repos/{repo}/issues/{number}/comments"),
            serde_json::json!({ "body": body }),
        )?;
        Ok(comment.into())
    }

    fn set_open(&self, repo: &RepoId, number: u64, open: bool) -> Result<Item> {
        let issue: WireIssue = self.send(
            "PATCH",
            &format!("/repos/{repo}/issues/{number}"),
            serde_json::json!({ "state": if open { "open" } else { "closed" } }),
        )?;
        issue
            .into_item(Some(repo))
            .ok_or_else(|| Error::Decode("item without a repository".into()))
    }

    fn merge(&self, repo: &RepoId, number: u64, method: MergeMethod) -> Result<()> {
        let _: serde_json::Value = self.send(
            "PUT",
            &format!("/repos/{repo}/pulls/{number}/merge"),
            serde_json::json!({ "merge_method": method.as_api() }),
        )?;
        Ok(())
    }

    fn checks(&self, repo: &RepoId, sha: &str) -> Result<Checks> {
        // Two kinds, both still in use: check runs (Actions and the apps)
        // and commit statuses (older integrations). One list, the runs
        // first.
        let (runs, _): (WireCheckRuns, _) = self.get(&format!(
            "/repos/{repo}/commits/{sha}/check-runs?per_page={PAGE_SIZE}"
        ))?;
        let (combined, _): (WireCombinedStatus, _) =
            self.get(&format!("/repos/{repo}/commits/{sha}/status"))?;
        let mut all: Vec<CheckRun> = runs.check_runs.into_iter().map(Into::into).collect();
        all.extend(combined.statuses.into_iter().map(Into::into));
        Ok(Checks { runs: all })
    }

    fn review_comments(&self, repo: &RepoId, number: u64) -> Result<Vec<ReviewComment>> {
        let path = format!("/repos/{repo}/pulls/{number}/comments?per_page={PAGE_SIZE}");
        let pages: Vec<WireReviewComment> = self.get_pages(&path)?;
        Ok(pages.into_iter().map(Into::into).collect())
    }

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
        let mut payload = serde_json::json!({
            "body": body,
            "commit_id": commit,
            "path": path,
            "line": line,
            "side": side.as_api(),
        });
        if let Some(start) = start.filter(|start| *start < line) {
            payload["start_line"] = start.into();
            payload["start_side"] = side.as_api().into();
        }
        let comment: WireReviewComment = self.send(
            "POST",
            &format!("/repos/{repo}/pulls/{number}/comments"),
            payload,
        )?;
        Ok(comment.into())
    }

    fn pull_checks(&self, keys: &[(RepoId, u64)]) -> Result<HashMap<(RepoId, u64), CheckState>> {
        // One GraphQL query per fifty pulls, each pull an aliased field:
        // the rollup on a head commit is what GitHub's own list shows, and
        // asking REST would be a round trip per row.
        let mut answer = HashMap::new();
        for chunk in keys.chunks(50) {
            let fields: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(index, (repo, number))| {
                    format!(
                        "p{index}: repository(owner: {}, name: {}) {{ pullRequest(number: {number}) {{ commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }} }} }}",
                        serde_json::json!(repo.owner),
                        serde_json::json!(repo.name),
                    )
                })
                .collect();
            let query = format!("query {{ {} }}", fields.join(" "));
            let data = self.graphql(&query, serde_json::json!({}))?;
            for (index, key) in chunk.iter().enumerate() {
                let state = data[format!("p{index}")]["pullRequest"]["commits"]["nodes"][0]
                    ["commit"]["statusCheckRollup"]["state"]
                    .as_str()
                    .map(rollup_state);
                if let Some(state) = state {
                    answer.insert(key.clone(), state);
                }
            }
        }
        Ok(answer)
    }

    fn set_draft(&self, node_id: &str, draft: bool) -> Result<()> {
        let query = if draft {
            r#"mutation($id: ID!) { convertPullRequestToDraft(input: {pullRequestId: $id}) { pullRequest { id } } }"#
        } else {
            r#"mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { id } } }"#
        };
        self.graphql(query, serde_json::json!({ "id": node_id }))?;
        Ok(())
    }

    fn commits(&self, repo: &RepoId, page: u32) -> Result<Vec<Commit>> {
        // One page, not the walk: the history is read as far as it is
        // scrolled, and the view asks for the next page when it gets there.
        let (commits, _): (Vec<WireCommit>, _) = self.get(&format!(
            "/repos/{repo}/commits?per_page={PAGE_SIZE}&page={page}"
        ))?;
        Ok(commits.into_iter().map(WireCommit::into_commit).collect())
    }

    fn commit(&self, repo: &RepoId, sha: &str) -> Result<CommitDetail> {
        let (commit, _): (WireCommit, _) = self.get(&format!("/repos/{repo}/commits/{sha}"))?;
        Ok(commit.into())
    }

    fn job(&self, repo: &RepoId, job_id: u64) -> Result<Job> {
        let (job, _): (WireJob, _) = self.get(&format!("/repos/{repo}/actions/jobs/{job_id}"))?;
        Ok(job.into())
    }

    fn job_log(&self, repo: &RepoId, job_id: u64) -> Result<String> {
        // GitHub answers with a redirect to the log itself, which ureq
        // follows; the body is plain text, not JSON.
        let (text, _) = self.fetch(&format!("/repos/{repo}/actions/jobs/{job_id}/logs"))?;
        Ok(text)
    }

    fn review(&self, repo: &RepoId, number: u64, event: ReviewEvent, body: &str) -> Result<()> {
        let mut payload = serde_json::json!({ "event": event.as_api() });
        if !body.trim().is_empty() {
            payload["body"] = serde_json::Value::String(body.to_string());
        }
        let _: serde_json::Value = self.send(
            "POST",
            &format!("/repos/{repo}/pulls/{number}/reviews"),
            payload,
        )?;
        Ok(())
    }

    fn labels(&self, repo: &RepoId) -> Result<Vec<Label>> {
        let pages: Vec<WireLabel> =
            self.get_pages(&format!("/repos/{repo}/labels?per_page={PAGE_SIZE}"))?;
        Ok(pages.into_iter().map(Into::into).collect())
    }

    fn add_labels(&self, repo: &RepoId, number: u64, labels: &[String]) -> Result<Item> {
        let _: serde_json::Value = self.send(
            "POST",
            &format!("/repos/{repo}/issues/{number}/labels"),
            serde_json::json!({ "labels": labels }),
        )?;
        self.item(repo, number)
    }

    fn remove_label(&self, repo: &RepoId, number: u64, label: &str) -> Result<Item> {
        let encoded = encode_query(label);
        let _: serde_json::Value = self.send(
            "DELETE",
            &format!("/repos/{repo}/issues/{number}/labels/{encoded}"),
            serde_json::json!({}),
        )?;
        self.item(repo, number)
    }

    fn assignees(&self, repo: &RepoId) -> Result<Vec<User>> {
        let pages: Vec<WireUser> =
            self.get_pages(&format!("/repos/{repo}/assignees?per_page={PAGE_SIZE}"))?;
        Ok(pages.into_iter().map(Into::into).collect())
    }

    fn add_assignees(&self, repo: &RepoId, number: u64, logins: &[String]) -> Result<Item> {
        let _: serde_json::Value = self.send(
            "POST",
            &format!("/repos/{repo}/issues/{number}/assignees"),
            serde_json::json!({ "assignees": logins }),
        )?;
        self.item(repo, number)
    }

    fn remove_assignees(&self, repo: &RepoId, number: u64, logins: &[String]) -> Result<Item> {
        let _: serde_json::Value = self.send(
            "DELETE",
            &format!("/repos/{repo}/issues/{number}/assignees"),
            serde_json::json!({ "assignees": logins }),
        )?;
        self.item(repo, number)
    }

    fn projects(&self, owner: &str) -> Result<Vec<Project>> {
        // An owner is a user or an organisation and the API will not say
        // which without a round trip; asking both in one query is cheaper
        // than asking which.
        let query = r#"query($login: String!) {
            user(login: $login) { projectsV2(first: 50, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { id title number closed url } } }
            organization(login: $login) { projectsV2(first: 50, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { id title number closed url } } }
        }"#;
        let data = self.graphql(query, serde_json::json!({ "login": owner }))?;
        let mut projects = Vec::new();
        for owner_kind in ["user", "organization"] {
            if let Some(nodes) = data[owner_kind]["projectsV2"]["nodes"].as_array() {
                for node in nodes {
                    projects.push(Project {
                        id: node["id"].as_str().unwrap_or_default().to_string(),
                        owner: owner.to_string(),
                        title: node["title"].as_str().unwrap_or_default().to_string(),
                        number: node["number"].as_u64().unwrap_or_default(),
                        closed: node["closed"].as_bool().unwrap_or(false),
                        html_url: node["url"].as_str().unwrap_or_default().to_string(),
                    });
                }
            }
        }
        Ok(projects)
    }

    fn all_projects(&self) -> Result<Vec<Project>> {
        // A person's Projects page includes projects owned by them and by
        // every organisation they belong to. Asking it as one graph avoids
        // one request per organisation and, unlike deriving owners from the
        // repository list, does not miss an organisation with no recent repo.
        let query = r#"query {
            viewer {
                login
                projectsV2(first: 100, orderBy: {field: UPDATED_AT, direction: DESC}) {
                    nodes { id title number closed url }
                }
                organizations(first: 100) {
                    nodes {
                        login
                        projectsV2(first: 100, orderBy: {field: UPDATED_AT, direction: DESC}) {
                            nodes { id title number closed url }
                        }
                    }
                }
            }
        }"#;
        let data = self.graphql(query, serde_json::json!({}))?;
        let viewer = &data["viewer"];
        let mut projects = Vec::new();
        let mut append = |owner: &str, nodes: Option<&Vec<serde_json::Value>>| {
            for node in nodes.into_iter().flatten() {
                projects.push(Project {
                    id: node["id"].as_str().unwrap_or_default().to_string(),
                    owner: owner.to_string(),
                    title: node["title"].as_str().unwrap_or_default().to_string(),
                    number: node["number"].as_u64().unwrap_or_default(),
                    closed: node["closed"].as_bool().unwrap_or(false),
                    html_url: node["url"].as_str().unwrap_or_default().to_string(),
                });
            }
        };
        append(
            viewer["login"].as_str().unwrap_or_default(),
            viewer["projectsV2"]["nodes"].as_array(),
        );
        if let Some(organizations) = viewer["organizations"]["nodes"].as_array() {
            for organization in organizations {
                append(
                    organization["login"].as_str().unwrap_or_default(),
                    organization["projectsV2"]["nodes"].as_array(),
                );
            }
        }
        Ok(projects)
    }

    fn project(&self, project: &Project) -> Result<ProjectBoard> {
        let mut board = ProjectBoard {
            project: project.clone(),
            items: Vec::new(),
            fields: Vec::new(),
            views: Vec::new(),
        };
        let mut cursor = None;
        loop {
            let page = self.project_page(project, cursor.as_deref())?;
            if board.fields.is_empty() {
                board.fields = page.board.fields;
                board.views = page.board.views;
            }
            board.items.extend(page.board.items);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(board);
            }
        }
    }

    fn project_page(&self, project: &Project, after: Option<&str>) -> Result<ProjectPage> {
        let query = r#"query($id: ID!, $after: String, $first: Int!, $includeMetadata: Boolean!) {
            node(id: $id) {
                ... on ProjectV2 {
                    fields(first: 100) @include(if: $includeMetadata) {
                        nodes {
                            __typename
                            ... on ProjectV2FieldCommon { id name dataType }
                            ... on ProjectV2SingleSelectField { options { id name color } }
                        }
                    }
                    views(first: 100) @include(if: $includeMetadata) {
                        nodes {
                            id name number layout filter
                            fields(first: 100) { nodes { ... on ProjectV2FieldCommon { id } } }
                            groupByFields(first: 1) { nodes { ... on ProjectV2FieldCommon { id } } }
                            verticalGroupByFields(first: 1) { nodes { ... on ProjectV2FieldCommon { id } } }
                        }
                    }
                    items(first: $first, after: $after) {
                        nodes {
                            id
                            isArchived
                            fieldValues(first: 100) {
                                nodes {
                                    __typename
                                    ... on ProjectV2ItemFieldSingleSelectValue {
                                        name field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldDateValue {
                                        date field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldIterationValue {
                                        title startDate duration field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldTextValue {
                                        text field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldNumberValue {
                                        number field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldLabelValue {
                                        labels(first: 10) { nodes { name } }
                                        field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldMilestoneValue {
                                        milestone { title }
                                        field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldUserValue {
                                        users(first: 10) { nodes { login } }
                                        field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                    ... on ProjectV2ItemFieldRepositoryValue {
                                        repository { nameWithOwner }
                                        field { ... on ProjectV2FieldCommon { id name } }
                                    }
                                }
                            }
                            content {
                                __typename
                                ... on DraftIssue { title }
                                ... on Issue { title number url state repository { nameWithOwner } }
                                ... on PullRequest { title number url state repository { nameWithOwner } }
                            }
                        }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        }"#;
        let mut items = Vec::new();
        let data = self.graphql(
            query,
            serde_json::json!({
                "id": project.id,
                "after": after,
                "first": if after.is_none() { PROJECT_FIRST_PAGE_SIZE } else { PAGE_SIZE },
                "includeMetadata": after.is_none(),
            }),
        )?;
        let node = &data["node"];
        let fields = node["fields"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|field| ProjectField {
                id: field["id"].as_str().unwrap_or_default().to_string(),
                name: field["name"].as_str().unwrap_or_default().to_string(),
                data_type: field["dataType"].as_str().unwrap_or_default().to_string(),
                options: field["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|option| ProjectFieldOption {
                        id: option["id"].as_str().unwrap_or_default().to_string(),
                        name: option["name"].as_str().unwrap_or_default().to_string(),
                        color: option["color"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect(),
            })
            .collect();
        let views = node["views"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|view| ProjectView {
                id: view["id"].as_str().unwrap_or_default().to_string(),
                name: view["name"].as_str().unwrap_or_default().to_string(),
                number: view["number"].as_u64().unwrap_or_default(),
                layout: match view["layout"].as_str() {
                    Some("BOARD_LAYOUT") => ProjectViewLayout::Board,
                    Some("ROADMAP_LAYOUT") => ProjectViewLayout::Roadmap,
                    _ => ProjectViewLayout::Table,
                },
                filter: view["filter"].as_str().map(str::to_string),
                group_by: view["groupByFields"]["nodes"]
                    .as_array()
                    .and_then(|nodes| nodes.first())
                    .and_then(|field| field["id"].as_str())
                    .map(str::to_string),
                vertical_group_by: view["verticalGroupByFields"]["nodes"]
                    .as_array()
                    .and_then(|nodes| nodes.first())
                    .and_then(|field| field["id"].as_str())
                    .map(str::to_string),
                visible_fields: view["fields"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|field| field["id"].as_str().map(str::to_string))
                    .collect(),
            })
            .collect();
        let connection = &node["items"];
        for node in connection["nodes"].as_array().into_iter().flatten() {
            let content = &node["content"];
            let kind = match content["__typename"].as_str() {
                Some("DraftIssue") => ProjectItemKind::DraftIssue,
                Some("Issue") => ProjectItemKind::Issue,
                Some("PullRequest") => ProjectItemKind::PullRequest,
                _ => ProjectItemKind::Redacted,
            };
            let repo = content["repository"]["nameWithOwner"]
                .as_str()
                .and_then(RepoId::parse);
            let item_fields: Vec<ProjectFieldValue> = node["fieldValues"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| {
                    let field_id = value["field"]["id"].as_str()?.to_string();
                    let field_name = value["field"]["name"].as_str()?.to_string();
                    let value = match value["__typename"].as_str()? {
                        "ProjectV2ItemFieldSingleSelectValue" => {
                            ProjectValue::SingleSelect(value["name"].as_str()?.to_string())
                        }
                        "ProjectV2ItemFieldDateValue" => {
                            ProjectValue::Date(value["date"].as_str()?.to_string())
                        }
                        "ProjectV2ItemFieldIterationValue" => ProjectValue::Iteration {
                            title: value["title"].as_str()?.to_string(),
                            start_date: value["startDate"].as_str()?.to_string(),
                            duration: value["duration"].as_u64()?,
                        },
                        "ProjectV2ItemFieldTextValue" => {
                            ProjectValue::Text(value["text"].as_str()?.to_string())
                        }
                        "ProjectV2ItemFieldNumberValue" => {
                            ProjectValue::Number(value["number"].as_f64()?.to_string())
                        }
                        "ProjectV2ItemFieldLabelValue" => ProjectValue::Names(
                            value["labels"]["nodes"]
                                .as_array()?
                                .iter()
                                .filter_map(|node| node["name"].as_str().map(str::to_string))
                                .collect(),
                        ),
                        "ProjectV2ItemFieldMilestoneValue" => ProjectValue::Names(vec![
                            value["milestone"]["title"].as_str()?.to_string(),
                        ]),
                        "ProjectV2ItemFieldUserValue" => ProjectValue::Names(
                            value["users"]["nodes"]
                                .as_array()?
                                .iter()
                                .filter_map(|node| node["login"].as_str().map(str::to_string))
                                .collect(),
                        ),
                        "ProjectV2ItemFieldRepositoryValue" => ProjectValue::Names(vec![
                            value["repository"]["nameWithOwner"].as_str()?.to_string(),
                        ]),
                        _ => return None,
                    };
                    Some(ProjectFieldValue {
                        field_id,
                        field_name,
                        value,
                    })
                })
                .collect();
            let status = item_fields
                .iter()
                .find_map(|field| (field.field_name == "Status").then_some(&field.value))
                .and_then(|value| match value {
                    ProjectValue::SingleSelect(name) => Some(name.clone()),
                    _ => None,
                });
            items.push(ProjectItem {
                id: node["id"].as_str().unwrap_or_default().to_string(),
                title: content["title"].as_str().unwrap_or_default().to_string(),
                kind,
                repo,
                number: content["number"].as_u64(),
                state: content["state"].as_str().map(str::to_string),
                status,
                fields: item_fields,
                html_url: content["url"].as_str().map(str::to_string),
                archived: node["isArchived"].as_bool().unwrap_or(false),
            });
        }
        let next_cursor = connection["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false)
            .then(|| {
                connection["pageInfo"]["endCursor"]
                    .as_str()
                    .map(str::to_string)
            })
            .flatten();
        Ok(ProjectPage {
            board: ProjectBoard {
                project: project.clone(),
                items,
                fields,
                views,
            },
            next_cursor,
        })
    }

    fn set_project_single_select(
        &self,
        project_id: &str,
        item_id: &str,
        field_id: &str,
        option_id: Option<&str>,
    ) -> Result<()> {
        let (query, variables) = if let Some(option_id) = option_id {
            (
                r#"mutation($project: ID!, $item: ID!, $field: ID!, $option: String!) {
                    updateProjectV2ItemFieldValue(input: {
                        projectId: $project, itemId: $item, fieldId: $field,
                        value: { singleSelectOptionId: $option }
                    }) { projectV2Item { id } }
                }"#,
                serde_json::json!({
                    "project": project_id,
                    "item": item_id,
                    "field": field_id,
                    "option": option_id,
                }),
            )
        } else {
            (
                r#"mutation($project: ID!, $item: ID!, $field: ID!) {
                    deleteProjectV2ItemFieldValue(input: {
                        projectId: $project, itemId: $item, fieldId: $field
                    }) { projectV2Item { id } }
                }"#,
                serde_json::json!({
                    "project": project_id,
                    "item": item_id,
                    "field": field_id,
                }),
            )
        };
        self.graphql(query, variables).map(|_| ())
    }

    fn item_projects(&self, repo: &RepoId, number: u64) -> Result<Vec<ProjectMembership>> {
        let query = r#"query($owner: String!, $name: String!, $number: Int!) {
            repository(owner: $owner, name: $name) {
                issueOrPullRequest(number: $number) {
                    ... on Issue { projectItems(first: 50) { nodes { id project { id title } } } }
                    ... on PullRequest { projectItems(first: 50) { nodes { id project { id title } } } }
                }
            }
        }"#;
        let data = self.graphql(
            query,
            serde_json::json!({ "owner": repo.owner, "name": repo.name, "number": number }),
        )?;
        let nodes = data["repository"]["issueOrPullRequest"]["projectItems"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(nodes
            .iter()
            .map(|node| ProjectMembership {
                project_id: node["project"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                title: node["project"]["title"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                item_id: node["id"].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    fn add_to_project(&self, project_id: &str, node_id: &str) -> Result<()> {
        let query = r#"mutation($project: ID!, $content: ID!) {
            addProjectV2ItemById(input: {projectId: $project, contentId: $content}) { item { id } }
        }"#;
        self.graphql(
            query,
            serde_json::json!({ "project": project_id, "content": node_id }),
        )?;
        Ok(())
    }

    fn remove_from_project(&self, project_id: &str, item_id: &str) -> Result<()> {
        let query = r#"mutation($project: ID!, $item: ID!) {
            deleteProjectV2Item(input: {projectId: $project, itemId: $item}) { deletedItemId }
        }"#;
        self.graphql(
            query,
            serde_json::json!({ "project": project_id, "item": item_id }),
        )?;
        Ok(())
    }

    fn file(&self, repo: &RepoId, path: &str) -> Result<FileContent> {
        let encoded: String = path
            .split('/')
            .map(encode_query)
            .collect::<Vec<_>>()
            .join("/");
        let (contents, _): (WireContents, _) =
            self.get(&format!("/repos/{repo}/contents/{encoded}"))?;
        Ok(contents.into_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_link_is_read_out_of_the_header() {
        let header = r#"<https://api.github.com/user/repos?page=2>; rel="next", <https://api.github.com/user/repos?page=5>; rel="last""#;
        assert_eq!(
            next_link(header).as_deref(),
            Some("https://api.github.com/user/repos?page=2")
        );
    }

    #[test]
    fn a_last_page_has_no_next() {
        let header = r#"<https://api.github.com/user/repos?page=1>; rel="prev", <https://api.github.com/user/repos?page=1>; rel="first""#;
        assert_eq!(next_link(header), None);
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn a_search_query_survives_the_url() {
        assert_eq!(
            encode_query("is:pr review-requested:@me state:open"),
            "is:pr+review-requested:%40me+state:open"
        );
    }

    #[test]
    fn the_reset_header_is_a_unix_timestamp() {
        assert_eq!(
            reset_at(1_788_566_400).map(|at| at.to_rfc3339()),
            Some("2026-09-05T00:00:00+00:00".to_string())
        );
    }

    #[test]
    fn a_listing_walks_beyond_three_pages() {
        let mut page = 0;
        let rows = walk_pages("page-1", |_| {
            page += 1;
            Ok::<_, ()>((vec![page], (page < 4).then(|| format!("page-{}", page + 1))))
        });
        assert_eq!(rows, Ok(vec![1, 2, 3, 4]));
    }
}
