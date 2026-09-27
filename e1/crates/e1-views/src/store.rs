//! Everything fetched, and the fetching of it.
//!
//! One entity holds every answer GitHub has given this window, each as a
//! [`Fetch`] so a refresh keeps the old value on screen. Every call goes to
//! the background executor and comes back through `this.update`; nothing
//! here blocks the UI thread, and nothing but this file calls the trait.
//!
//! Two caches make it fast. The HTTP layer keeps answers with their
//! `ETag`s (`e1_github::HttpCache`), so a refresh GitHub answers with `304`
//! costs a round trip and no rate limit. And the store writes what it knows
//! to a [`Snapshot`] after every answer, and reads it back before the first
//! request on the next launch, so the window opens full and revalidates
//! rather than opening empty and waiting.

use e1_github::{
    CheckState, Checks, Commit, CommitDetail, FileContent, GitHub, Item, Job, Label, ListKind,
    MergeMethod, Notification, Project, ProjectBoard, ProjectFieldValue, ProjectMembership,
    ProjectValue, PullFile, Repo, RepoId, ReviewComment, ReviewEvent, Side, Tree, User, Viewer,
};
use e1_ui::fetch::describe;
use e1_ui::snapshot::{self, ItemDetail, Snapshot};
use e1_ui::{Fetch, Focus, Section};
use gpui::{AppContext as _, Context, EventEmitter};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

/// The key of a detail: which item, in which repository.
pub type ItemKey = (RepoId, u64);

/// The key of a file: which path, in which repository.
pub type FileKey = (RepoId, String);

/// Everything the detail panel draws for one item.
pub type Detail = ItemDetail;

/// Emitted whenever an answer lands.
pub enum StoreEvent {
    /// Something changed; views re-read what they show.
    Changed,
}

/// The window's memory of GitHub.
pub struct Store {
    github: Arc<dyn GitHub>,
    viewer: Fetch<Viewer>,
    repos: Fetch<Vec<Repo>>,
    inbox: Fetch<Vec<Notification>>,
    lists: HashMap<Focus, Fetch<Vec<Item>>>,
    details: HashMap<ItemKey, Fetch<Detail>>,
    /// The order details were opened in, oldest first, for the snapshot.
    opened: Vec<ItemKey>,
    pull_files: HashMap<ItemKey, Fetch<Vec<PullFile>>>,
    trees: HashMap<RepoId, Fetch<Tree>>,
    contents: HashMap<FileKey, Fetch<FileContent>>,
    /// Where the snapshot is written, when it is.
    snapshot: Option<PathBuf>,
    /// Where avatars are kept, when they are.
    avatar_dir: Option<PathBuf>,
    /// Each avatar URL's file, once fetched.
    avatars: HashMap<String, Fetch<PathBuf>>,
    /// The last write on each item: in flight, done, or refused.
    actions: HashMap<ItemKey, Fetch<()>>,
    /// Each repository's labels, for the picker.
    repo_labels: HashMap<RepoId, Fetch<Vec<Label>>>,
    /// Each repository's assignable people, for the picker.
    candidates: HashMap<RepoId, Fetch<Vec<User>>>,
    /// Each owner's projects, for the picker.
    projects: HashMap<String, Fetch<Vec<Project>>>,
    /// Every project visible to the viewer, for the fixed sidebar section.
    all_projects: Fetch<Vec<Project>>,
    /// Project contents, by the project's global id.
    project_boards: HashMap<String, Fetch<ProjectBoard>>,
    /// Complete Projects in least-recently-opened order for the snapshot.
    opened_projects: Vec<String>,
    /// The last complete Project answers. Pagination may replace the visible
    /// value with a partial board, but the disk cache must never do so.
    project_cache: HashMap<String, ProjectBoard>,
    /// The current paginated load generation for each Project. A refresh
    /// supersedes callbacks still arriving from its previous cursor chain.
    project_loads: HashMap<String, u64>,
    /// The last drag between Project columns, by project id.
    project_actions: HashMap<String, Fetch<()>>,
    /// Which projects each item is in.
    memberships: HashMap<ItemKey, Fetch<Vec<ProjectMembership>>>,
    /// The checks on each commit, by repository and sha.
    checks: HashMap<(RepoId, String), Fetch<Checks>>,
    /// The comments on each pull's diff.
    review_comments: HashMap<ItemKey, Fetch<Vec<ReviewComment>>>,
    /// Each Actions job's log, by repository and job id.
    logs: HashMap<(RepoId, u64), Fetch<String>>,
    /// Each Actions job's steps, by repository and job id.
    jobs: HashMap<(RepoId, u64), Fetch<Job>>,
    /// How the checks stand on each pull a list has shown.
    statuses: HashMap<(RepoId, u64), CheckState>,
    /// Each repository's history, once it has been asked for. Grown a page
    /// at a time as the reader scrolls back.
    commits: HashMap<RepoId, Fetch<Vec<Commit>>>,
    /// How many pages of each history have landed.
    commit_pages: HashMap<RepoId, u32>,
    /// The histories that have given everything they have.
    commits_ended: HashSet<RepoId>,
    /// Each commit that has been read, by repository and hash.
    commit_details: HashMap<(RepoId, String), Fetch<CommitDetail>>,
    /// The coding-agent CLIs on this machine, once they have been looked
    /// for. Empty until then, and empty is also the answer when there are
    /// none.
    agents: Vec<e1_ui::agents::Agent>,
    /// Which of them an ask goes to. Remembered in the settings, so it
    /// arrives here from the window rather than being decided here.
    chosen: Option<e1_ui::agents::Kind>,
    /// How each of them is asked: which model, and how much thinking. Per
    /// CLI, because the names are, and remembered in the settings the same
    /// way the choice of CLI is.
    tuning: HashMap<e1_ui::agents::Kind, e1_ui::agents::Tuning>,
}

impl EventEmitter<StoreEvent> for Store {}

impl Store {
    /// A store over a source. Nothing is fetched until asked.
    pub fn new(github: Arc<dyn GitHub>) -> Self {
        Self {
            github,
            viewer: Fetch::Idle,
            repos: Fetch::Idle,
            inbox: Fetch::Idle,
            lists: HashMap::new(),
            details: HashMap::new(),
            opened: Vec::new(),
            pull_files: HashMap::new(),
            trees: HashMap::new(),
            contents: HashMap::new(),
            snapshot: None,
            avatar_dir: None,
            avatars: HashMap::new(),
            actions: HashMap::new(),
            repo_labels: HashMap::new(),
            candidates: HashMap::new(),
            projects: HashMap::new(),
            all_projects: Fetch::Idle,
            project_boards: HashMap::new(),
            opened_projects: Vec::new(),
            project_cache: HashMap::new(),
            project_loads: HashMap::new(),
            project_actions: HashMap::new(),
            memberships: HashMap::new(),
            checks: HashMap::new(),
            review_comments: HashMap::new(),
            logs: HashMap::new(),
            jobs: HashMap::new(),
            statuses: HashMap::new(),
            commits: HashMap::new(),
            commit_pages: HashMap::new(),
            commits_ended: HashSet::new(),
            commit_details: HashMap::new(),
            agents: Vec::new(),
            chosen: None,
            tuning: HashMap::new(),
        }
    }

    /// The CLI an ask goes to: the one that was chosen, if it is still
    /// installed, and otherwise the first one found.
    pub fn chosen_agent(&self) -> Option<&e1_ui::agents::Agent> {
        self.chosen
            .and_then(|kind| {
                self.agents
                    .iter()
                    .find(|agent| agent.kind == kind && agent.kind.supports_chat())
            })
            .or_else(|| self.agents.iter().find(|agent| agent.kind.supports_chat()))
    }

    /// Whether the installed CLIs include one whose session e1 can resume.
    pub fn has_chat_agent(&self) -> bool {
        self.agents.iter().any(|agent| agent.kind.supports_chat())
    }

    /// Remember which CLI an ask goes to.
    pub fn choose_agent(&mut self, kind: e1_ui::agents::Kind, cx: &mut Context<Self>) {
        self.chosen = Some(kind);
        cx.notify();
    }

    /// How an ask to this CLI is put: the model it was last given and how
    /// much thinking it was allowed. Nothing chosen is the usual answer,
    /// and it means the CLI starts on its own configuration.
    pub fn tuning(&self, kind: e1_ui::agents::Kind) -> e1_ui::agents::Tuning {
        self.tuning.get(&kind).cloned().unwrap_or_default()
    }

    /// Remember how this CLI is asked.
    pub fn tune(
        &mut self,
        kind: e1_ui::agents::Kind,
        tuning: e1_ui::agents::Tuning,
        cx: &mut Context<Self>,
    ) {
        self.tuning.insert(kind, tuning);
        cx.notify();
    }

    /// The coding-agent CLIs that were found on this machine.
    pub fn agents(&self) -> &[e1_ui::agents::Agent] {
        &self.agents
    }

    /// Look for the agent CLIs, once, in the background.
    ///
    /// Not a `fetch`: this asks the machine rather than GitHub, and it is
    /// slow enough to matter — a login shell and a process per CLI — so it
    /// happens off the window's thread and lands whenever it lands.
    pub fn load_agents(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { e1_ui::agents::discover() })
                .await;
            this.update(cx, |this, cx| {
                tracing::info!(count = found.len(), "agent CLIs found");
                this.agents = found;
                cx.emit(StoreEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A repository's history, if it has ever been asked for.
    pub fn commits(&self, repo: &RepoId) -> Option<&Fetch<Vec<Commit>>> {
        self.commits.get(repo)
    }

    /// Fetch a repository's history from the top, forgetting what was read
    /// before: what a refresh does.
    pub fn load_commits(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        self.commit_pages.remove(&repo);
        self.commits_ended.remove(&repo);
        self.commits.entry(repo.clone()).or_default().begin();
        self.read_commits(repo, 1, cx);
    }

    /// Fetch a repository's history only if it never has been.
    pub fn ensure_commits(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.commits.get(&repo).is_none_or(Fetch::is_idle) {
            self.load_commits(repo, cx);
        }
    }

    /// Whether the whole history has been read.
    pub fn commits_ended(&self, repo: &RepoId) -> bool {
        self.commits_ended.contains(repo)
    }

    /// Read the next page of a history, if there is one and none is on its
    /// way. What the view asks for when the reader reaches the bottom.
    pub fn more_commits(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        let reading = self
            .commits
            .get(&repo)
            .is_some_and(|fetch| fetch.is_loading());
        if reading || self.commits_ended.contains(&repo) {
            return;
        }
        let read = self.commit_pages.get(&repo).copied().unwrap_or_default();
        if read == 0 {
            return self.ensure_commits(repo, cx);
        }
        self.commits.entry(repo.clone()).or_default().begin();
        self.read_commits(repo, read + 1, cx);
    }

    /// One page, appended to what is already there.
    fn read_commits(&mut self, repo: RepoId, page: u32, cx: &mut Context<Self>) {
        let key = repo.clone();
        self.fetch(
            cx,
            move |github| github.commits(&repo, page),
            move |this, result, _| {
                match result {
                    Ok(more) => {
                        // A page shorter than a full one is the last: there
                        // is no count to ask for and no cursor to follow.
                        if more.len() < e1_github::PAGE_SIZE {
                            this.commits_ended.insert(key.clone());
                        }
                        tracing::debug!(page, read = more.len(), "a page of history");
                        this.commit_pages.insert(key.clone(), page);
                        let held = this.commits.entry(key).or_default();
                        let mut all = held.value().cloned().unwrap_or_default();
                        if page == 1 {
                            all.clear();
                        }
                        all.extend(more);
                        held.finish(Ok(all));
                    }
                    Err(error) => {
                        this.commits.entry(key).or_default().finish(Err(error));
                    }
                }
            },
        );
    }

    /// One commit with its files, if it has ever been asked for.
    pub fn commit(&self, repo: &RepoId, sha: &str) -> Option<&Fetch<CommitDetail>> {
        self.commit_details.get(&(repo.clone(), sha.to_string()))
    }

    /// Fetch one commit with its files.
    pub fn load_commit(&mut self, repo: RepoId, sha: String, cx: &mut Context<Self>) {
        let key = (repo.clone(), sha.clone());
        self.commit_details.entry(key.clone()).or_default().begin();
        self.fetch(
            cx,
            move |github| github.commit(&repo, &sha),
            move |this, result, _| {
                this.commit_details.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch one commit only if it never has been.
    pub fn ensure_commit(&mut self, repo: RepoId, sha: String, cx: &mut Context<Self>) {
        if self
            .commit_details
            .get(&(repo.clone(), sha.clone()))
            .is_none_or(Fetch::is_idle)
        {
            self.load_commit(repo, sha, cx);
        }
    }

    /// How the checks stand on a pull, once a list has asked.
    pub fn status(&self, key: &ItemKey) -> Option<CheckState> {
        self.statuses.get(key).copied()
    }

    /// Fetch how the checks stand on these pulls, in one round trip.
    fn load_statuses(&mut self, keys: Vec<ItemKey>, cx: &mut Context<Self>) {
        if keys.is_empty() {
            return;
        }
        self.fetch(
            cx,
            move |github| github.pull_checks(&keys),
            move |this, result, _| {
                if let Ok(statuses) = result {
                    this.statuses.extend(statuses);
                }
            },
        );
    }

    /// A job's steps, if they have ever been asked for.
    pub fn job(&self, repo: &RepoId, job: u64) -> Option<&Fetch<Job>> {
        self.jobs.get(&(repo.clone(), job))
    }

    /// Fetch a job's steps.
    pub fn load_job(&mut self, repo: RepoId, job: u64, cx: &mut Context<Self>) {
        let key = (repo.clone(), job);
        self.jobs.entry(key.clone()).or_default().begin();
        self.fetch(
            cx,
            move |github| github.job(&repo, job),
            move |this, result, _| {
                this.jobs.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch a job's steps only if they never have been.
    pub fn ensure_job(&mut self, repo: RepoId, job: u64, cx: &mut Context<Self>) {
        if self
            .jobs
            .get(&(repo.clone(), job))
            .is_none_or(Fetch::is_idle)
        {
            self.load_job(repo, job, cx);
        }
    }

    /// The comments on a pull's diff, if they have ever been asked for.
    pub fn review_comments(&self, key: &ItemKey) -> Option<&Fetch<Vec<ReviewComment>>> {
        self.review_comments.get(key)
    }

    /// Fetch the comments on a pull's diff.
    pub fn load_review_comments(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        self.review_comments.entry(key.clone()).or_default().begin();
        let (repo, number) = key.clone();
        self.fetch(
            cx,
            move |github| github.review_comments(&repo, number),
            move |this, result, _| {
                this.review_comments.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch the comments on a pull's diff only if they never have been.
    pub fn ensure_review_comments(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        if self.review_comments.get(&key).is_none_or(Fetch::is_idle) {
            self.load_review_comments(key, cx);
        }
    }

    /// A job's log, if it has ever been asked for.
    pub fn log(&self, repo: &RepoId, job: u64) -> Option<&Fetch<String>> {
        self.logs.get(&(repo.clone(), job))
    }

    /// Fetch a job's log.
    pub fn load_log(&mut self, repo: RepoId, job: u64, cx: &mut Context<Self>) {
        let key = (repo.clone(), job);
        self.logs.entry(key.clone()).or_default().begin();
        self.fetch(
            cx,
            move |github| github.job_log(&repo, job),
            move |this, result, _| {
                this.logs.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch a job's log only if it never has been.
    pub fn ensure_log(&mut self, repo: RepoId, job: u64, cx: &mut Context<Self>) {
        if self
            .logs
            .get(&(repo.clone(), job))
            .is_none_or(Fetch::is_idle)
        {
            self.load_log(repo, job, cx);
        }
    }

    /// Comment on a line of a pull's diff.
    #[allow(clippy::too_many_arguments)]
    pub fn review_comment(
        &mut self,
        key: ItemKey,
        commit: String,
        path: String,
        start: Option<u32>,
        line: u32,
        side: Side,
        body: String,
        cx: &mut Context<Self>,
    ) {
        let reload = key.clone();
        self.act_then(
            key,
            move |github, repo, number| {
                github
                    .review_comment(repo, number, &commit, &path, start, line, side, &body)
                    .map(|_| ())
            },
            move |this, cx| this.load_review_comments(reload, cx),
            cx,
        );
    }

    /// Mark the pull a draft, or ready for review.
    pub fn set_draft(&mut self, key: ItemKey, draft: bool, cx: &mut Context<Self>) {
        let Some(node_id) = self
            .details
            .get(&key)
            .and_then(Fetch::value)
            .map(|detail| detail.item.node_id.clone())
        else {
            return;
        };
        self.act(
            key,
            move |github, _, _| github.set_draft(&node_id, draft),
            cx,
        );
    }

    /// The checks on a commit, if they have ever been asked for.
    pub fn checks(&self, repo: &RepoId, sha: &str) -> Option<&Fetch<Checks>> {
        self.checks.get(&(repo.clone(), sha.to_string()))
    }

    /// Fetch the checks on a commit.
    pub fn load_checks(&mut self, repo: RepoId, sha: String, cx: &mut Context<Self>) {
        if sha.is_empty() {
            return;
        }
        let key = (repo.clone(), sha.clone());
        self.checks.entry(key.clone()).or_default().begin();
        self.fetch(
            cx,
            move |github| github.checks(&repo, &sha),
            move |this, result, _| {
                this.checks.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch the checks on a commit only if they never have been.
    pub fn ensure_checks(&mut self, repo: RepoId, sha: String, cx: &mut Context<Self>) {
        if self
            .checks
            .get(&(repo.clone(), sha.clone()))
            .is_none_or(Fetch::is_idle)
        {
            self.load_checks(repo, sha, cx);
        }
    }

    /// A repository's labels, if they have ever been asked for.
    pub fn repo_labels(&self, repo: &RepoId) -> Option<&Fetch<Vec<Label>>> {
        self.repo_labels.get(repo)
    }

    /// Fetch a repository's labels only if they never have been.
    pub fn ensure_repo_labels(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.repo_labels.get(&repo).is_none_or(Fetch::is_idle) {
            self.repo_labels.entry(repo.clone()).or_default().begin();
            let key = repo.clone();
            self.fetch(
                cx,
                move |github| github.labels(&repo),
                move |this, result, _| {
                    this.repo_labels.entry(key).or_default().finish(result);
                },
            );
        }
    }

    /// A repository's assignable people, if they have ever been asked for.
    pub fn candidates(&self, repo: &RepoId) -> Option<&Fetch<Vec<User>>> {
        self.candidates.get(repo)
    }

    /// Fetch a repository's assignable people only if they never have been.
    pub fn ensure_candidates(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.candidates.get(&repo).is_none_or(Fetch::is_idle) {
            self.candidates.entry(repo.clone()).or_default().begin();
            let key = repo.clone();
            self.fetch(
                cx,
                move |github| github.assignees(&repo),
                move |this, result, _| {
                    this.candidates.entry(key).or_default().finish(result);
                },
            );
        }
    }

    /// An owner's projects, if they have ever been asked for.
    pub fn projects(&self, owner: &str) -> Option<&Fetch<Vec<Project>>> {
        self.projects.get(owner)
    }

    /// Every project visible to the signed-in viewer.
    pub fn all_projects(&self) -> &Fetch<Vec<Project>> {
        &self.all_projects
    }

    /// Fetch every personal and organisation project the viewer can reach.
    pub fn load_all_projects(&mut self, cx: &mut Context<Self>) {
        self.all_projects.begin();
        self.fetch(
            cx,
            |github| github.all_projects(),
            |this, result, _| this.all_projects.finish(result),
        );
    }

    /// One project's contents, if they have ever been asked for.
    pub fn project_board(&self, id: &str) -> Option<&Fetch<ProjectBoard>> {
        self.project_boards.get(id)
    }

    /// Fetch one project's contents.
    pub fn load_project(&mut self, project: Project, cx: &mut Context<Self>) {
        self.opened_projects.retain(|id| id != &project.id);
        self.opened_projects.push(project.id.clone());
        let generation = {
            let generation = self.project_loads.entry(project.id.clone()).or_default();
            *generation = generation.wrapping_add(1);
            *generation
        };
        if self.project_cache.contains_key(&project.id) {
            let id = project.id.clone();
            self.project_boards.entry(id.clone()).or_default().begin();
            self.fetch_transient(
                cx,
                {
                    let project = project.clone();
                    move |github| github.project(&project)
                },
                move |this, result, cx| {
                    if this.project_loads.get(&id) != Some(&generation) {
                        return;
                    }
                    if let Ok(board) = &result {
                        this.project_cache.insert(id.clone(), board.clone());
                    }
                    this.project_boards.entry(id).or_default().finish(result);
                    this.persist(cx);
                },
            );
        } else {
            self.load_project_page(project, None, true, generation, cx);
        }
    }

    /// Fetch one Project page, publish it immediately, then continue with the
    /// following cursor. This keeps a large board usable while the rest lands.
    fn load_project_page(
        &mut self,
        project: Project,
        after: Option<String>,
        replace: bool,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let id = project.id.clone();
        self.project_boards.entry(id.clone()).or_default().begin();
        self.fetch_transient(
            cx,
            {
                let project = project.clone();
                let after = after.clone();
                move |github| github.project_page(&project, after.as_deref())
            },
            move |this, result, cx| match result {
                Ok(page) => {
                    if this.project_loads.get(&id) != Some(&generation) {
                        return;
                    }
                    let next = page.next_cursor.clone();
                    let board = if replace {
                        page.board
                    } else {
                        let mut board = this
                            .project_boards
                            .get(&id)
                            .and_then(Fetch::value)
                            .cloned()
                            .unwrap_or_else(|| ProjectBoard {
                                project: project.clone(),
                                items: Vec::new(),
                                fields: Vec::new(),
                                views: Vec::new(),
                            });
                        e1_ui::project::append_page(&mut board, page);
                        board
                    };
                    this.project_boards
                        .entry(id.clone())
                        .or_default()
                        .finish(Ok(board));
                    if let Some(next) = next {
                        this.load_project_page(project, Some(next), false, generation, cx);
                    } else {
                        if let Some(board) =
                            this.project_boards.get(&id).and_then(Fetch::value).cloned()
                        {
                            this.project_cache.insert(id.clone(), board);
                        }
                        this.persist(cx);
                    }
                }
                Err(error) => {
                    if this.project_loads.get(&id) != Some(&generation) {
                        return;
                    }
                    this.project_boards
                        .entry(id)
                        .or_default()
                        .finish(Err(error));
                }
            },
        );
    }

    /// Fetch one project's contents only if it has not been read yet.
    pub fn ensure_project(&mut self, project: Project, cx: &mut Context<Self>) {
        if !self.project_loads.contains_key(&project.id)
            || self
                .project_boards
                .get(&project.id)
                .is_none_or(Fetch::is_idle)
        {
            self.load_project(project, cx);
        }
    }

    /// The last field update made by dragging a Project card.
    pub fn project_action(&self, project_id: &str) -> Option<&Fetch<()>> {
        self.project_actions.get(project_id)
    }

    /// Move a Project card to a single-select column, optimistically, then
    /// reconcile it with GitHub's answer.
    pub fn move_project_item(
        &mut self,
        project: Project,
        item_id: String,
        field_id: String,
        option: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if let Some(fetch) = self.project_boards.get_mut(&project.id)
            && let Some(mut board) = fetch.value().cloned()
            && let Some(item) = board.items.iter_mut().find(|item| item.id == item_id)
        {
            item.fields.retain(|value| value.field_id != field_id);
            if let Some((_, name)) = option.as_ref() {
                let field_name = board
                    .fields
                    .iter()
                    .find(|field| field.id == field_id)
                    .map(|field| field.name.clone())
                    .unwrap_or_default();
                item.fields.push(ProjectFieldValue {
                    field_id: field_id.clone(),
                    field_name: field_name.clone(),
                    value: ProjectValue::SingleSelect(name.clone()),
                });
                if field_name.eq_ignore_ascii_case("status") {
                    item.status = Some(name.clone());
                }
            } else if board
                .fields
                .iter()
                .find(|field| field.id == field_id)
                .is_some_and(|field| field.name.eq_ignore_ascii_case("status"))
            {
                item.status = None;
            }
            *fetch = Fetch::Ready(board);
            if let Some(board) = fetch.value().cloned() {
                self.project_cache.insert(project.id.clone(), board);
            }
        }
        self.project_actions
            .entry(project.id.clone())
            .or_default()
            .begin();
        let option_id = option.map(|(id, _)| id);
        let project_id = project.id.clone();
        self.fetch(
            cx,
            move |github| {
                github.set_project_single_select(
                    &project_id,
                    &item_id,
                    &field_id,
                    option_id.as_deref(),
                )
            },
            move |this, result, cx| {
                this.project_actions
                    .entry(project.id.clone())
                    .or_default()
                    .finish(result);
                this.load_project(project, cx);
            },
        );
    }

    /// Fetch an owner's projects, keeping a stale answer on screen while
    /// GitHub revalidates it.
    pub fn load_projects(&mut self, owner: String, cx: &mut Context<Self>) {
        self.projects.entry(owner.clone()).or_default().begin();
        let key = owner.clone();
        self.fetch(
            cx,
            move |github| github.projects(&owner),
            move |this, result, _| {
                this.projects.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch an owner's projects only if they never have been.
    pub fn ensure_projects(&mut self, owner: String, cx: &mut Context<Self>) {
        if self.projects.get(&owner).is_none_or(Fetch::is_idle) {
            self.load_projects(owner, cx);
        }
    }

    /// The projects an item is in, if they have ever been asked for.
    pub fn memberships(&self, key: &ItemKey) -> Option<&Fetch<Vec<ProjectMembership>>> {
        self.memberships.get(key)
    }

    /// Fetch the projects an item is in.
    pub fn load_memberships(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        self.memberships.entry(key.clone()).or_default().begin();
        let (repo, number) = key.clone();
        self.fetch(
            cx,
            move |github| github.item_projects(&repo, number),
            move |this, result, _| {
                this.memberships.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch the projects an item is in only if they never have been.
    pub fn ensure_memberships(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        if self.memberships.get(&key).is_none_or(Fetch::is_idle) {
            self.load_memberships(key, cx);
        }
    }

    /// Keep avatars in this directory.
    pub fn with_avatars(mut self, dir: PathBuf) -> Self {
        self.avatar_dir = Some(dir);
        self
    }

    /// The file an avatar is in, once it is.
    pub fn avatar(&self, url: &str) -> Option<PathBuf> {
        self.avatars.get(url).and_then(Fetch::value).cloned()
    }

    /// Fetch an avatar unless it is here already, on disk or in flight.
    ///
    /// The file is named by a hash of the URL and read from disk first: a
    /// picture fetched last week is a picture, and GitHub's avatar URLs are
    /// stable per account.
    pub fn ensure_avatar(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(dir) = self.avatar_dir.clone() else {
            return;
        };
        if url.is_empty() || self.avatars.contains_key(url) {
            return;
        }
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        let path = dir.join(format!("{:016x}.img", hasher.finish()));
        if path.exists() {
            self.avatars.insert(url.to_string(), Fetch::Ready(path));
            return;
        }
        self.avatars
            .insert(url.to_string(), Fetch::Loading { stale: None });
        let key = url.to_string();
        let source = url.to_string();
        self.fetch(
            cx,
            move |github| {
                let bytes = github.avatar(&source)?;
                std::fs::create_dir_all(&dir)
                    .map_err(|e| e1_github::Error::Transport(e.to_string()))?;
                let temp = path.with_extension("img.tmp");
                std::fs::write(&temp, bytes)
                    .map_err(|e| e1_github::Error::Transport(e.to_string()))?;
                std::fs::rename(&temp, &path)
                    .map_err(|e| e1_github::Error::Transport(e.to_string()))?;
                Ok(path)
            },
            move |this, result, _| {
                this.avatars.entry(key).or_default().finish(result);
            },
        );
    }

    /// The last write on an item, if there was one.
    pub fn action(&self, key: &ItemKey) -> Option<&Fetch<()>> {
        self.actions.get(key)
    }

    /// Run a write on an item, then read the item again so the screen shows
    /// what GitHub now says rather than what the window guessed.
    fn act<W>(&mut self, key: ItemKey, work: W, cx: &mut Context<Self>)
    where
        W: FnOnce(&dyn GitHub, &RepoId, u64) -> e1_github::Result<()> + Send + 'static,
    {
        self.actions.entry(key.clone()).or_default().begin();
        let (repo, number) = key.clone();
        self.fetch(
            cx,
            move |github| work(github, &repo, number),
            move |this, result, cx| {
                let ok = result.is_ok();
                this.actions.entry(key.clone()).or_default().finish(result);
                if ok {
                    // The write's answer is partial (a comment, a state);
                    // the detail is the whole, and the cache makes the
                    // re-read cheap.
                    this.load_detail(key, None, cx);
                }
            },
        );
    }

    /// Leave a comment.
    pub fn comment_on(&mut self, key: ItemKey, body: String, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.comment_on(repo, number, &body).map(|_| ()),
            cx,
        );
    }

    /// Close an item, or open it again.
    pub fn set_open(&mut self, key: ItemKey, open: bool, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.set_open(repo, number, open).map(|_| ()),
            cx,
        );
    }

    /// Merge a pull, one of three ways.
    pub fn merge(&mut self, key: ItemKey, method: MergeMethod, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.merge(repo, number, method),
            cx,
        );
    }

    /// Review a pull.
    pub fn review(
        &mut self,
        key: ItemKey,
        event: ReviewEvent,
        body: String,
        cx: &mut Context<Self>,
    ) {
        self.act(
            key,
            move |github, repo, number| github.review(repo, number, event, &body),
            cx,
        );
    }

    /// Put a label on an item.
    pub fn add_label(&mut self, key: ItemKey, name: String, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.add_labels(repo, number, &[name]).map(|_| ()),
            cx,
        );
    }

    /// Take a label off an item.
    pub fn remove_label(&mut self, key: ItemKey, name: String, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.remove_label(repo, number, &name).map(|_| ()),
            cx,
        );
    }

    /// Assign someone.
    pub fn add_assignee(&mut self, key: ItemKey, login: String, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.add_assignees(repo, number, &[login]).map(|_| ()),
            cx,
        );
    }

    /// Unassign someone.
    pub fn remove_assignee(&mut self, key: ItemKey, login: String, cx: &mut Context<Self>) {
        self.act(
            key,
            move |github, repo, number| github.remove_assignees(repo, number, &[login]).map(|_| ()),
            cx,
        );
    }

    /// Put an item into a project. The item's global id comes from the
    /// detail already on screen.
    pub fn add_to_project(&mut self, key: ItemKey, project_id: String, cx: &mut Context<Self>) {
        let Some(node_id) = self
            .details
            .get(&key)
            .and_then(Fetch::value)
            .map(|detail| detail.item.node_id.clone())
        else {
            return;
        };
        let reload = key.clone();
        self.act_then(
            key,
            move |github, _, _| github.add_to_project(&project_id, &node_id),
            move |this, cx| this.load_memberships(reload, cx),
            cx,
        );
    }

    /// Take an item out of a project.
    pub fn remove_from_project(
        &mut self,
        key: ItemKey,
        project_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        let reload = key.clone();
        self.act_then(
            key,
            move |github, _, _| github.remove_from_project(&project_id, &item_id),
            move |this, cx| this.load_memberships(reload, cx),
            cx,
        );
    }

    /// Like [`Store::act`], with something more to do once it lands.
    fn act_then<W, T>(&mut self, key: ItemKey, work: W, then: T, cx: &mut Context<Self>)
    where
        W: FnOnce(&dyn GitHub, &RepoId, u64) -> e1_github::Result<()> + Send + 'static,
        T: FnOnce(&mut Self, &mut Context<Self>) + 'static,
    {
        self.actions.entry(key.clone()).or_default().begin();
        let (repo, number) = key.clone();
        self.fetch(
            cx,
            move |github| work(github, &repo, number),
            move |this, result, cx| {
                let ok = result.is_ok();
                this.actions.entry(key.clone()).or_default().finish(result);
                if ok {
                    this.load_detail(key, None, cx);
                    then(this, cx);
                }
            },
        );
    }

    /// Remember what lands at this path, and start from what is there.
    pub fn with_snapshot(mut self, path: PathBuf) -> Self {
        if let Some(snapshot) = snapshot::load(&path) {
            self.adopt(snapshot);
        }
        self.remembering(path)
    }

    /// Remember what lands at this path, without reading what is there:
    /// for a window that opens signed out, whose snapshot — if one survived
    /// — belongs to whoever was signed in before.
    pub fn remembering(mut self, path: PathBuf) -> Self {
        self.snapshot = Some(path);
        self
    }

    /// Take a snapshot's contents as what is on screen. They are `Ready`
    /// rather than stale so that the first refresh keeps them, the way a
    /// refresh keeps anything.
    fn adopt(&mut self, snapshot: Snapshot) {
        if let Some(viewer) = snapshot.viewer {
            self.viewer = Fetch::Ready(viewer);
        }
        if !snapshot.repos.is_empty() {
            self.repos = Fetch::Ready(snapshot.repos);
        }
        self.inbox = Fetch::Ready(snapshot.inbox);
        if !snapshot.projects.is_empty() {
            self.all_projects = Fetch::Ready(snapshot.projects);
        }
        for (focus, items) in snapshot.lists {
            self.lists.insert(focus, Fetch::Ready(items));
        }
        for (key, detail) in snapshot.details {
            self.opened.push(key.clone());
            self.details.insert(key, Fetch::Ready(detail));
        }
        for board in snapshot.project_boards {
            let id = board.project.id.clone();
            self.opened_projects.push(id.clone());
            self.project_cache.insert(id.clone(), board.clone());
            self.project_boards.insert(id, Fetch::Ready(board));
        }
    }

    /// What the next launch should open on.
    fn snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot::new();
        snapshot.viewer = self.viewer.value().cloned();
        snapshot.repos = self.repos.value().cloned().unwrap_or_default();
        snapshot.inbox = self.inbox.value().cloned().unwrap_or_default();
        snapshot.lists = self
            .lists
            .iter()
            .filter_map(|(focus, fetch)| fetch.value().map(|items| (focus.clone(), items.clone())))
            .collect();
        snapshot.details = self
            .opened
            .iter()
            .filter_map(|key| {
                self.details
                    .get(key)
                    .and_then(Fetch::value)
                    .map(|detail| (key.clone(), detail.clone()))
            })
            .collect();
        snapshot.projects = self.all_projects.value().cloned().unwrap_or_default();
        snapshot.project_boards = self
            .opened_projects
            .iter()
            .filter_map(|id| self.project_cache.get(id).cloned())
            .collect();
        snapshot.trim();
        snapshot
    }

    /// Write the snapshot, off the UI thread.
    fn persist(&self, cx: &mut Context<Self>) {
        let Some(path) = self.snapshot.clone() else {
            return;
        };
        let snapshot = self.snapshot();
        cx.background_spawn(async move {
            if let Err(error) = snapshot::save(&path, &snapshot) {
                tracing::debug!(%error, "could not write the snapshot");
            }
        })
        .detach();
    }

    /// Who the token is.
    pub fn viewer(&self) -> &Fetch<Viewer> {
        &self.viewer
    }

    /// The repositories.
    pub fn repos(&self) -> &Fetch<Vec<Repo>> {
        &self.repos
    }

    /// The inbox.
    pub fn inbox(&self) -> &Fetch<Vec<Notification>> {
        &self.inbox
    }

    /// A list, if it has ever been asked for.
    pub fn list(&self, focus: &Focus) -> Option<&Fetch<Vec<Item>>> {
        self.lists.get(focus)
    }

    /// A detail, if it has ever been asked for.
    pub fn detail(&self, key: &ItemKey) -> Option<&Fetch<Detail>> {
        self.details.get(key)
    }

    /// A pull's files, if they have ever been asked for.
    pub fn pull_files(&self, key: &ItemKey) -> Option<&Fetch<Vec<PullFile>>> {
        self.pull_files.get(key)
    }

    /// A repository's tree, if it has ever been asked for.
    pub fn tree(&self, repo: &RepoId) -> Option<&Fetch<Tree>> {
        self.trees.get(repo)
    }

    /// A file, if it has ever been asked for.
    pub fn content(&self, key: &FileKey) -> Option<&Fetch<FileContent>> {
        self.contents.get(key)
    }

    /// Swap the source and forget everything the old one said.
    ///
    /// Signing in and out: a token change is a different GitHub, and a list
    /// fetched as one person must not be shown to the next. Everything is
    /// fetched again from the new source, and the snapshot goes too.
    pub fn set_source(&mut self, github: Arc<dyn GitHub>, cx: &mut Context<Self>) {
        self.github = github;
        self.viewer = Fetch::Idle;
        self.repos = Fetch::Idle;
        self.inbox = Fetch::Idle;
        self.lists.clear();
        self.projects.clear();
        self.all_projects = Fetch::Idle;
        self.project_boards.clear();
        self.opened_projects.clear();
        self.project_cache.clear();
        self.project_loads.clear();
        self.project_actions.clear();
        self.details.clear();
        self.opened.clear();
        self.pull_files.clear();
        self.trees.clear();
        self.contents.clear();
        if let Some(path) = self.snapshot.clone() {
            cx.background_spawn(async move {
                if let Err(error) = snapshot::forget(&path) {
                    tracing::warn!(%error, "could not delete the snapshot");
                }
            })
            .detach();
        }
        self.refresh_all(cx);
        cx.emit(StoreEvent::Changed);
    }

    /// Fetch what the sidebar needs: the viewer, the repositories and the
    /// inbox count.
    pub fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.load_viewer(cx);
        self.load_repos(cx);
        self.load_inbox(cx);
        self.load_all_projects(cx);
    }

    /// Fetch the viewer.
    pub fn load_viewer(&mut self, cx: &mut Context<Self>) {
        self.viewer.begin();
        self.fetch(
            cx,
            |github| github.viewer(),
            |this, result, _| this.viewer.finish(result),
        );
    }

    /// Fetch the repositories.
    pub fn load_repos(&mut self, cx: &mut Context<Self>) {
        self.repos.begin();
        self.fetch(
            cx,
            |github| github.repositories(),
            |this, result, _| this.repos.finish(result),
        );
    }

    /// Fetch the inbox.
    pub fn load_inbox(&mut self, cx: &mut Context<Self>) {
        self.inbox.begin();
        self.fetch(
            cx,
            |github| github.notifications(),
            |this, result, cx| {
                let pulls: Vec<ItemKey> = result
                    .as_deref()
                    .map(|inbox| {
                        inbox
                            .iter()
                            .filter(|notification| {
                                notification.kind == e1_github::SubjectKind::PullRequest
                            })
                            .filter_map(|notification| {
                                notification
                                    .number
                                    .map(|number| (notification.repo.clone(), number))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                this.inbox.finish(result);
                this.load_statuses(pulls, cx);
            },
        );
    }

    /// Fetch a list, whether or not it has been fetched before. The file
    /// finder is not a list; see [`Store::load_tree`].
    pub fn load_list(&mut self, focus: Focus, cx: &mut Context<Self>) {
        if focus == Focus::Section(Section::Inbox) {
            return self.load_inbox(cx);
        }
        if focus == Focus::Section(Section::Projects) {
            self.load_all_projects(cx);
            return;
        }
        if !focus.is_list() {
            return;
        }
        self.lists.entry(focus.clone()).or_default().begin();
        let key = focus.clone();
        self.fetch(
            cx,
            move |github| match &focus {
                Focus::Section(Section::Projects) => Ok(Vec::new()),
                Focus::Section(section) => github.search(section.query().unwrap_or_default()),
                Focus::Search { query } => github.search(query),
                Focus::Repo { repo, kind, status } => github.items(repo, *kind, *status),
                Focus::Files { .. } | Focus::History { .. } => Ok(Vec::new()),
            },
            move |this, result, cx| {
                let pulls: Vec<ItemKey> = result
                    .as_deref()
                    .map(|items| {
                        items
                            .iter()
                            .filter(|item| item.is_pull())
                            .map(|item| (item.repo.clone(), item.number))
                            .collect()
                    })
                    .unwrap_or_default();
                this.lists.entry(key).or_default().finish(result);
                this.load_statuses(pulls, cx);
            },
        );
    }

    /// Fetch a list only if it never has been. What a view calls when it
    /// starts showing one: a list already on screen is not re-fetched by
    /// looking at it again.
    pub fn ensure_list(&mut self, focus: Focus, cx: &mut Context<Self>) {
        let idle = match &focus {
            Focus::Section(Section::Inbox) => self.inbox.is_idle(),
            Focus::Section(Section::Projects) => self.all_projects.is_idle(),
            other => self.lists.get(other).is_none_or(Fetch::is_idle),
        };
        if idle {
            self.load_list(focus, cx);
        }
    }

    /// Fetch an item, what only a pull has, and its comments, together.
    ///
    /// `is_pull` is a hint from the row that opened it: knowing saves the
    /// round trip that would otherwise find out. `None` asks.
    pub fn load_detail(&mut self, key: ItemKey, is_pull: Option<bool>, cx: &mut Context<Self>) {
        self.details.entry(key.clone()).or_default().begin();
        self.opened.retain(|opened| opened != &key);
        self.opened.push(key.clone());
        let (repo, number) = key.clone();
        self.fetch(
            cx,
            move |github| {
                let (item, pull) = match is_pull {
                    Some(true) => {
                        let pull = github.pull(&repo, number)?;
                        (pull.item.clone(), Some(pull))
                    }
                    Some(false) => (github.item(&repo, number)?, None),
                    None => {
                        let item = github.item(&repo, number)?;
                        let pull = if item.is_pull() {
                            Some(github.pull(&repo, number)?)
                        } else {
                            None
                        };
                        (item, pull)
                    }
                };
                let comments = github.comments(&repo, number)?;
                Ok(Detail {
                    item,
                    pull,
                    comments,
                })
            },
            move |this, result, _| {
                this.details.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch a detail only if it never has been.
    pub fn ensure_detail(&mut self, key: ItemKey, is_pull: Option<bool>, cx: &mut Context<Self>) {
        if self.details.get(&key).is_none_or(Fetch::is_idle) {
            self.load_detail(key, is_pull, cx);
        }
    }

    /// Fetch a pull's files.
    pub fn load_pull_files(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        self.pull_files.entry(key.clone()).or_default().begin();
        let (repo, number) = key.clone();
        self.fetch(
            cx,
            move |github| github.pull_files(&repo, number),
            move |this, result, _| {
                this.pull_files.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch a pull's files only if they never have been.
    pub fn ensure_pull_files(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        if self.pull_files.get(&key).is_none_or(Fetch::is_idle) {
            self.load_pull_files(key, cx);
        }
    }

    /// Fetch a repository's tree.
    pub fn load_tree(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        self.trees.entry(repo.clone()).or_default().begin();
        let key = repo.clone();
        self.fetch(
            cx,
            move |github| github.tree(&repo),
            move |this, result, _| {
                this.trees.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch a repository's tree only if it never has been.
    pub fn ensure_tree(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.trees.get(&repo).is_none_or(Fetch::is_idle) {
            self.load_tree(repo, cx);
        }
    }

    /// Fetch a file.
    pub fn load_content(&mut self, key: FileKey, cx: &mut Context<Self>) {
        self.contents.entry(key.clone()).or_default().begin();
        let (repo, path) = key.clone();
        self.fetch(
            cx,
            move |github| github.file(&repo, &path),
            move |this, result, _| {
                this.contents.entry(key).or_default().finish(result);
            },
        );
    }

    /// Fetch a file only if it never has been.
    pub fn ensure_content(&mut self, key: FileKey, cx: &mut Context<Self>) {
        if self.contents.get(&key).is_none_or(Fetch::is_idle) {
            self.load_content(key, cx);
        }
    }

    /// The kind of list a focus is, for the row that opens an item out of it.
    pub fn kind_of(focus: &Focus) -> Option<ListKind> {
        match focus {
            Focus::Repo { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Run `work` against the source on a background thread, then `apply`
    /// its answer on the UI thread, tell every view, and write the snapshot.
    ///
    /// The one place a trait call happens. The answer's error is turned into
    /// the reader's sentence here rather than in a view, because a view that
    /// formats errors is a view that has to know the error type.
    fn fetch<T, W, A>(&self, cx: &mut Context<Self>, work: W, apply: A)
    where
        T: Send + 'static,
        W: FnOnce(&dyn GitHub) -> e1_github::Result<T> + Send + 'static,
        A: FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    {
        self.fetch_inner(cx, true, work, apply);
    }

    /// Run a request whose intermediate answer must not rewrite the disk
    /// snapshot. A paginated Project persists once, after its final page.
    fn fetch_transient<T, W, A>(&self, cx: &mut Context<Self>, work: W, apply: A)
    where
        T: Send + 'static,
        W: FnOnce(&dyn GitHub) -> e1_github::Result<T> + Send + 'static,
        A: FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    {
        self.fetch_inner(cx, false, work, apply);
    }

    fn fetch_inner<T, W, A>(&self, cx: &mut Context<Self>, persist: bool, work: W, apply: A)
    where
        T: Send + 'static,
        W: FnOnce(&dyn GitHub) -> e1_github::Result<T> + Send + 'static,
        A: FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    {
        cx.notify();
        let github = self.github.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    work(github.as_ref()).map_err(|error| {
                        tracing::warn!(%error, "GitHub request failed");
                        describe(&error)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                apply(this, result, cx);
                cx.emit(StoreEvent::Changed);
                cx.notify();
                if persist {
                    this.persist(cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
