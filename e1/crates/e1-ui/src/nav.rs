//! What the sidebar offers, and what the centre column is showing.

use e1_github::{ListKind, Repo, RepoId, StatusFilter};
use serde::{Deserialize, Serialize};

/// The fixed rows at the top of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Section {
    /// Unread notifications.
    Inbox,
    /// Open pulls the viewer authored.
    MyPulls,
    /// Open pulls waiting for the viewer's review.
    Reviews,
    /// Open issues assigned to the viewer.
    Assigned,
    /// The viewer's GitHub Projects.
    Projects,
}

impl Section {
    /// All fixed rows, in sidebar order.
    pub const ALL: &'static [Section] = &[
        Section::Inbox,
        Section::MyPulls,
        Section::Reviews,
        Section::Assigned,
        Section::Projects,
    ];

    /// The locale key for the row's label.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Inbox => "sidebar.inbox",
            Self::MyPulls => "sidebar.my_pulls",
            Self::Reviews => "sidebar.reviews",
            Self::Assigned => "sidebar.assigned",
            Self::Projects => "sidebar.projects",
        }
    }

    /// The row's icon, as an asset path.
    pub fn icon(self) -> &'static str {
        use crate::assets::icon;
        match self {
            Self::Inbox => icon::INBOX,
            Self::MyPulls => icon::PULL_REQUEST,
            Self::Reviews => icon::EYE,
            Self::Assigned => icon::USER_CHECK,
            Self::Projects => icon::PROJECT,
        }
    }

    /// The search that lists an item section, or `None` for a section with
    /// its own endpoint. `@me` rather than the login keeps the query equal
    /// to what a person would type into GitHub.
    pub fn query(self) -> Option<&'static str> {
        match self {
            Self::Inbox | Self::Projects => None,
            Self::MyPulls => Some("is:pr is:open author:@me"),
            Self::Reviews => Some("is:pr is:open review-requested:@me"),
            Self::Assigned => Some("is:issue is:open assignee:@me"),
        }
    }
}

/// What the centre column lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Focus {
    /// One of the fixed sections.
    Section(Section),
    /// A repository's pulls or issues.
    Repo {
        /// Which repository.
        repo: RepoId,
        /// Pulls or issues.
        kind: ListKind,
        /// Open, closed, or all.
        status: StatusFilter,
    },
    /// A repository's files, found by path.
    Files {
        /// Which repository.
        repo: RepoId,
    },
    /// A repository's commits.
    History {
        /// Which repository.
        repo: RepoId,
    },
    /// Items matching a search the reader typed.
    Search {
        /// The query, in GitHub's search syntax.
        query: String,
    },
}

/// The three things the centre column can show for a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoTab {
    /// Pull requests.
    Pulls,
    /// Issues.
    Issues,
    /// The file tree.
    Files,
    /// The commit history.
    History,
}

impl RepoTab {
    /// All four, in the order the chips show them.
    pub const ALL: &'static [RepoTab] = &[
        RepoTab::Pulls,
        RepoTab::Issues,
        RepoTab::Files,
        RepoTab::History,
    ];

    /// The locale key for the chip.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Pulls => "list.pulls",
            Self::Issues => "list.issues",
            Self::Files => "list.files",
            Self::History => "list.history",
        }
    }
}

impl Focus {
    /// A repository's file finder.
    pub fn files(repo: RepoId) -> Self {
        Self::Files { repo }
    }

    /// A repository's history.
    pub fn history(repo: RepoId) -> Self {
        Self::History { repo }
    }

    /// A search.
    pub fn search(query: impl Into<String>) -> Self {
        Self::Search {
            query: query.into(),
        }
    }

    /// Which tab a repository focus is on, or `None` for anything else.
    pub fn repo_tab(&self) -> Option<RepoTab> {
        match self {
            Self::Repo { kind, .. } => Some(match kind {
                ListKind::Pulls => RepoTab::Pulls,
                ListKind::Issues => RepoTab::Issues,
            }),
            Self::Files { .. } => Some(RepoTab::Files),
            Self::History { .. } => Some(RepoTab::History),
            Self::Section(_) | Self::Search { .. } => None,
        }
    }

    /// The same repository, on another tab. The status is kept when both
    /// tabs have one and starts open otherwise.
    pub fn with_tab(&self, tab: RepoTab) -> Option<Self> {
        let repo = self.repo_id()?.clone();
        let status = match self {
            Self::Repo { status, .. } => *status,
            _ => StatusFilter::Open,
        };
        Some(match tab {
            RepoTab::Pulls => Self::Repo {
                repo,
                kind: ListKind::Pulls,
                status,
            },
            RepoTab::Issues => Self::Repo {
                repo,
                kind: ListKind::Issues,
                status,
            },
            RepoTab::Files => Self::Files { repo },
            RepoTab::History => Self::History { repo },
        })
    }

    /// Whether the store answers this focus with a list of items. The file
    /// tree and the history are their own things; a search is a list.
    pub fn is_list(&self) -> bool {
        !matches!(self, Self::Files { .. } | Self::History { .. })
    }

    /// A repository's open pulls, which is what picking a repository shows
    /// first.
    pub fn repo(repo: RepoId) -> Self {
        Self::Repo {
            repo,
            kind: ListKind::Pulls,
            status: StatusFilter::Open,
        }
    }

    /// The same focus with another kind, keeping the repository.
    pub fn with_kind(&self, kind: ListKind) -> Option<Self> {
        match self {
            Self::Repo { repo, status, .. } => Some(Self::Repo {
                repo: repo.clone(),
                kind,
                status: *status,
            }),
            _ => None,
        }
    }

    /// The same focus with another status, keeping the repository.
    pub fn with_status(&self, status: StatusFilter) -> Option<Self> {
        match self {
            Self::Repo { repo, kind, .. } => Some(Self::Repo {
                repo: repo.clone(),
                kind: *kind,
                status,
            }),
            _ => None,
        }
    }

    /// The repository, when the focus is one.
    pub fn repo_id(&self) -> Option<&RepoId> {
        match self {
            Self::Repo { repo, .. } | Self::Files { repo } | Self::History { repo } => Some(repo),
            Self::Section(_) | Self::Search { .. } => None,
        }
    }

    /// What the centre strip says.
    pub fn title(&self) -> String {
        match self {
            Self::Section(section) => rust_i18n::t!(section.label_key()).to_string(),
            Self::Repo { repo, .. } | Self::Files { repo } | Self::History { repo } => {
                repo.to_string()
            }
            Self::Search { query } => query.clone(),
        }
    }

    /// What the centre strip says after the title, when there is more.
    pub fn subtitle(&self) -> Option<String> {
        match self {
            Self::Section(_) => None,
            Self::Repo { .. } | Self::Files { .. } | Self::History { .. } => self
                .repo_tab()
                .map(|tab| rust_i18n::t!(tab.label_key()).to_string()),
            Self::Search { .. } => Some(rust_i18n::t!("search.title").to_string()),
        }
    }
}

/// One owner's repositories, for the sidebar's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerGroup {
    /// The user or organisation.
    pub owner: String,
    /// Its repositories, in the order they were given.
    pub repos: Vec<Repo>,
}

/// Group repositories under their owner, in order of first appearance.
///
/// The list arrives most recently pushed first, so an owner's place in the
/// tree is where its busiest repository is, and the repositories inside
/// keep that order too. A person with one organisation sees it at the top;
/// one with ten sees the ones that moved this week first.
pub fn group_by_owner(repos: &[Repo]) -> Vec<OwnerGroup> {
    let mut groups: Vec<OwnerGroup> = Vec::new();
    for repo in repos {
        match groups.iter_mut().find(|group| group.owner == repo.id.owner) {
            Some(group) => group.repos.push(repo.clone()),
            None => groups.push(OwnerGroup {
                owner: repo.id.owner.clone(),
                repos: vec![repo.clone()],
            }),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> Repo {
        Repo {
            id: RepoId::new(owner, name),
            description: None,
            private: false,
            default_branch: "main".into(),
            stars: 0,
            open_issues: 0,
            pushed_at: None,
            html_url: String::new(),
        }
    }

    #[test]
    fn owners_are_ordered_by_their_first_repository_and_keep_their_own_order() {
        let repos = [
            repo("acme", "web"),
            repo("bokuweb", "e1"),
            repo("acme", "api"),
            repo("bokuweb", "ginka"),
        ];
        let groups = group_by_owner(&repos);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].owner, "acme");
        assert_eq!(
            groups[0]
                .repos
                .iter()
                .map(|r| r.id.name.as_str())
                .collect::<Vec<_>>(),
            ["web", "api"]
        );
        assert_eq!(groups[1].owner, "bokuweb");
        assert_eq!(groups[1].repos.len(), 2);
        assert!(group_by_owner(&[]).is_empty());
    }

    #[test]
    fn item_sections_are_searches_but_inbox_and_projects_are_not() {
        assert_eq!(Section::Inbox.query(), None);
        assert_eq!(Section::Projects.query(), None);
        for section in Section::ALL
            .iter()
            .filter(|s| !matches!(s, Section::Inbox | Section::Projects))
        {
            let query = section.query().unwrap();
            assert!(query.contains("is:open"), "{section:?}: {query}");
            assert!(query.contains("@me"), "{section:?}: {query}");
        }
    }

    #[test]
    fn picking_a_repository_shows_its_open_pulls_first() {
        let focus = Focus::repo(RepoId::new("o", "r"));
        assert_eq!(
            focus,
            Focus::Repo {
                repo: RepoId::new("o", "r"),
                kind: ListKind::Pulls,
                status: StatusFilter::Open
            }
        );
        let issues = focus.with_kind(ListKind::Issues).unwrap();
        assert_eq!(issues.repo_id(), Some(&RepoId::new("o", "r")));
        let closed = issues.with_status(StatusFilter::Closed).unwrap();
        assert!(matches!(
            closed,
            Focus::Repo {
                kind: ListKind::Issues,
                status: StatusFilter::Closed,
                ..
            }
        ));
    }

    #[test]
    fn the_tabs_walk_a_repository_and_keep_the_status_between_the_two_lists() {
        let repo = RepoId::new("o", "r");
        let closed = Focus::repo(repo.clone())
            .with_status(StatusFilter::Closed)
            .unwrap();
        let files = closed.with_tab(RepoTab::Files).unwrap();
        assert_eq!(files, Focus::files(repo.clone()));
        assert_eq!(files.repo_tab(), Some(RepoTab::Files));
        assert!(!files.is_list());
        // Back to a list: open, because the finder had no status to keep.
        let issues = files.with_tab(RepoTab::Issues).unwrap();
        assert!(matches!(
            issues,
            Focus::Repo {
                status: StatusFilter::Open,
                ..
            }
        ));
        // Between the two lists the status survives.
        let issues = closed.with_tab(RepoTab::Issues).unwrap();
        assert!(matches!(
            issues,
            Focus::Repo {
                kind: ListKind::Issues,
                status: StatusFilter::Closed,
                ..
            }
        ));
        assert_eq!(
            Focus::Section(Section::Inbox).with_tab(RepoTab::Files),
            None
        );
    }

    #[test]
    fn a_search_is_a_list_titled_by_its_query() {
        rust_i18n::set_locale("en");
        let focus = Focus::search("is:pr label:bug");
        assert!(focus.is_list());
        assert_eq!(focus.title(), "is:pr label:bug");
        assert_eq!(focus.subtitle().as_deref(), Some("Search"));
        assert_eq!(focus.repo_id(), None);
    }

    #[test]
    fn a_section_has_no_kind_to_switch() {
        assert_eq!(
            Focus::Section(Section::Inbox).with_kind(ListKind::Issues),
            None
        );
        assert_eq!(Focus::Section(Section::Inbox).repo_id(), None);
    }

    #[test]
    fn the_strip_says_what_the_list_is() {
        rust_i18n::set_locale("en");
        assert_eq!(Focus::Section(Section::Reviews).title(), "Reviews");
        let focus = Focus::repo(RepoId::new("bokuweb", "e1"));
        assert_eq!(focus.title(), "bokuweb/e1");
        assert_eq!(focus.subtitle().as_deref(), Some("Pull requests"));
    }
}
