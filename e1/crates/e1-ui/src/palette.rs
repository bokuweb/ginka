//! What the palette offers for what has been typed (`docs/ui.md` §3.6).
//!
//! The palette is one field over everywhere the window can go: the four
//! fixed sections, the repositories the viewer can see, and — always last —
//! GitHub's own issue search for the words themselves. Deciding which rows
//! those are is the part with a decision in it, so it lives here rather than
//! in the view (`AGENTS.md` rule 6), matched by the same `nucleo` the file
//! finder uses.

use crate::finder;
use crate::nav::Section;
use e1_github::{Repo, RepoId};

/// What picking a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Point the window at one of the fixed sections.
    Section(Section),
    /// Point it at a repository's pull requests.
    Repo(RepoId),
    /// Ask GitHub's issue search for the words themselves.
    Search(String),
}

/// The heading a run of rows sits under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// The fixed sidebar rows.
    Sections,
    /// The repositories the viewer can see.
    Repositories,
    /// The one row that costs a request.
    Search,
}

impl Group {
    /// The locale key for the heading.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Sections => "palette.sections",
            Self::Repositories => "sidebar.repositories",
            Self::Search => "palette.search",
        }
    }
}

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What picking it does.
    pub pick: Pick,
    /// The row's own words.
    pub label: String,
    /// What follows the label, muted: the owner a repository sits under,
    /// which is what tells two repositories of the same name apart.
    pub detail: Option<String>,
    /// Whether the row carries the lock a private repository carries in the
    /// sidebar.
    pub private: bool,
}

/// A heading and the rows under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Which heading.
    pub group: Group,
    /// Its rows, in the order they are drawn.
    pub rows: Vec<Row>,
}

/// What the palette offers for `query`, at most `cap` repositories.
///
/// An empty query is the window's own furniture — the sections, then the
/// repositories in the sidebar's order — because a palette that offered
/// nothing until something was typed would be a search box with extra steps.
/// Anything typed is matched fuzzily against both, and the search row is
/// added at the end: it is the one row that leaves for the network, so it is
/// what is left when nothing here matches rather than the first thing
/// offered.
pub fn offer(repos: &[Repo], query: &str, cap: usize) -> Vec<Block> {
    let query = query.trim();
    let mut blocks = Vec::new();

    let labels: Vec<String> = Section::ALL
        .iter()
        .map(|section| rust_i18n::t!(section.label_key()).to_string())
        .collect();
    let sections: Vec<Row> = finder::find(&labels, query, Section::ALL.len())
        .into_iter()
        .map(|index| Row {
            pick: Pick::Section(Section::ALL[index]),
            label: labels[index].clone(),
            detail: None,
            private: false,
        })
        .collect();
    if !sections.is_empty() {
        blocks.push(Block {
            group: Group::Sections,
            rows: sections,
        });
    }

    // Matched on `owner/name` rather than on the name alone, so that typing
    // an owner lists everything under it, the way the sidebar groups them.
    let full_names: Vec<String> = repos.iter().map(|repo| repo.id.to_string()).collect();
    let matched: Vec<Row> = finder::find(&full_names, query, cap)
        .into_iter()
        .map(|index| Row {
            pick: Pick::Repo(repos[index].id.clone()),
            label: repos[index].id.name.clone(),
            detail: Some(repos[index].id.owner.clone()),
            private: repos[index].private,
        })
        .collect();
    if !matched.is_empty() {
        blocks.push(Block {
            group: Group::Repositories,
            rows: matched,
        });
    }

    if !query.is_empty() {
        blocks.push(Block {
            group: Group::Search,
            rows: vec![Row {
                pick: Pick::Search(query.to_string()),
                label: rust_i18n::t!("palette.search_for", query = query).to_string(),
                detail: None,
                private: false,
            }],
        });
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str, private: bool) -> Repo {
        Repo {
            id: RepoId::new(owner, name),
            description: None,
            private,
            default_branch: "main".into(),
            stars: 0,
            open_issues: 0,
            pushed_at: None,
            html_url: String::new(),
        }
    }

    fn repos() -> Vec<Repo> {
        vec![
            repo("bokuweb", "e1", false),
            repo("bokuweb", "ginka", false),
            repo("acme", "secrets", true),
        ]
    }

    fn setup() {
        rust_i18n::set_locale("en");
    }

    fn picks(blocks: &[Block], group: Group) -> Vec<Pick> {
        blocks
            .iter()
            .find(|block| block.group == group)
            .map(|block| block.rows.iter().map(|row| row.pick.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn an_empty_query_is_the_sections_then_the_repositories_in_order() {
        setup();
        let blocks = offer(&repos(), "", 10);
        assert_eq!(
            blocks.iter().map(|block| block.group).collect::<Vec<_>>(),
            vec![Group::Sections, Group::Repositories]
        );
        assert_eq!(
            picks(&blocks, Group::Sections),
            Section::ALL
                .iter()
                .copied()
                .map(Pick::Section)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            picks(&blocks, Group::Repositories),
            vec![
                Pick::Repo(RepoId::new("bokuweb", "e1")),
                Pick::Repo(RepoId::new("bokuweb", "ginka")),
                Pick::Repo(RepoId::new("acme", "secrets")),
            ]
        );
    }

    #[test]
    fn a_query_matches_a_repository_by_name_or_by_owner() {
        setup();
        let blocks = offer(&repos(), "ginka", 10);
        assert_eq!(
            picks(&blocks, Group::Repositories),
            vec![Pick::Repo(RepoId::new("bokuweb", "ginka"))]
        );
        let blocks = offer(&repos(), "bokuweb", 10);
        assert_eq!(
            picks(&blocks, Group::Repositories),
            vec![
                Pick::Repo(RepoId::new("bokuweb", "e1")),
                Pick::Repo(RepoId::new("bokuweb", "ginka")),
            ]
        );
    }

    #[test]
    fn a_query_matches_a_section_by_its_label() {
        setup();
        let blocks = offer(&repos(), "inbox", 10);
        assert_eq!(
            picks(&blocks, Group::Sections),
            vec![Pick::Section(Section::Inbox)]
        );
    }

    #[test]
    fn the_search_row_is_last_and_only_once_something_is_typed() {
        setup();
        assert!(picks(&offer(&repos(), "", 10), Group::Search).is_empty());
        let blocks = offer(&repos(), "flaky test", 10);
        assert_eq!(blocks.last().map(|block| block.group), Some(Group::Search));
        assert_eq!(
            picks(&blocks, Group::Search),
            vec![Pick::Search("flaky test".into())]
        );
    }

    #[test]
    fn nothing_local_matches_leaves_the_search_row_alone() {
        setup();
        let blocks = offer(&repos(), "zzzqqq", 10);
        assert_eq!(
            blocks.iter().map(|block| block.group).collect::<Vec<_>>(),
            vec![Group::Search]
        );
    }

    #[test]
    fn a_repository_carries_its_owner_and_its_lock() {
        setup();
        let blocks = offer(&repos(), "secrets", 10);
        let block = blocks
            .iter()
            .find(|block| block.group == Group::Repositories)
            .expect("the repository matched");
        assert_eq!(block.rows[0].label, "secrets");
        assert_eq!(block.rows[0].detail.as_deref(), Some("acme"));
        assert!(block.rows[0].private);
    }

    #[test]
    fn the_cap_holds_over_the_repositories() {
        setup();
        assert_eq!(picks(&offer(&repos(), "", 2), Group::Repositories).len(), 2);
    }
}
