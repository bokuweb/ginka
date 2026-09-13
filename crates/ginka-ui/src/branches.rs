//! Testable branch-picker policy.

use ginka_protocol::model::BranchInfo;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::PathBuf;

/// Whether choosing a branch can change the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAvailability {
    /// This workspace already has the branch checked out.
    Current,
    /// Git permits this workspace to switch to the branch.
    Available,
    /// Another worktree owns the branch, at the daemon-host path carried here.
    CheckedOutAt(PathBuf),
}

/// One branch after filtering and ordering for the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchOption {
    /// The daemon's live description of the branch.
    pub branch: BranchInfo,
    /// What clicking this row is allowed to do.
    pub availability: BranchAvailability,
}

/// What Return in the branch search will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchSubmission {
    /// Close the picker because the best match is already current.
    KeepCurrent,
    /// Switch to an existing available branch.
    Switch(String),
    /// Create the normalized query as a new branch.
    Create(String),
}

/// Filter local branches fuzzily, keeping current and usable choices ahead of
/// branches held by another worktree.
pub fn matching_branches(branches: &[BranchInfo], query: &str) -> Vec<BranchOption> {
    let query = query.trim();
    let pattern = (!query.is_empty())
        .then(|| Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart));
    let mut options = branches
        .iter()
        .filter_map(|branch| {
            let score = pattern.as_ref().map_or(Some(0), |pattern| {
                let mut matcher = Matcher::new(Config::DEFAULT);
                let mut buffer = Vec::new();
                let name = nucleo_matcher::Utf32Str::new(&branch.name, &mut buffer);
                pattern.score(name, &mut matcher)
            })?;
            let availability = if branch.current {
                BranchAvailability::Current
            } else if let Some(path) = &branch.checked_out_at {
                BranchAvailability::CheckedOutAt(path.clone())
            } else {
                BranchAvailability::Available
            };
            Some((
                score,
                BranchOption {
                    branch: branch.clone(),
                    availability,
                },
            ))
        })
        .collect::<Vec<_>>();
    options.sort_by(|(left_score, left), (right_score, right)| {
        availability_rank(&left.availability)
            .cmp(&availability_rank(&right.availability))
            .then_with(|| right_score.cmp(left_score))
            .then_with(|| left.branch.name.cmp(&right.branch.name))
    });
    options.into_iter().map(|(_, option)| option).collect()
}

/// A normalized new branch name when the query is non-empty and does not name
/// an existing local branch.
pub fn new_branch_candidate(branches: &[BranchInfo], query: &str) -> Option<String> {
    let candidate = query.trim();
    (!candidate.is_empty() && !branches.iter().any(|branch| branch.name == candidate))
        .then(|| candidate.to_string())
}

/// Resolve Return to the best fuzzy existing match before offering creation.
pub fn branch_submission(branches: &[BranchInfo], query: &str) -> Option<BranchSubmission> {
    if let Some(option) = matching_branches(branches, query).into_iter().next() {
        return match option.availability {
            BranchAvailability::Current => Some(BranchSubmission::KeepCurrent),
            BranchAvailability::Available => Some(BranchSubmission::Switch(option.branch.name)),
            BranchAvailability::CheckedOutAt(_) => None,
        };
    }
    new_branch_candidate(branches, query).map(BranchSubmission::Create)
}

fn availability_rank(availability: &BranchAvailability) -> u8 {
    match availability {
        BranchAvailability::Current => 0,
        BranchAvailability::Available => 1,
        BranchAvailability::CheckedOutAt(_) => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, current: bool, held: Option<&str>) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            current,
            checked_out_at: held.map(PathBuf::from),
        }
    }

    #[test]
    fn the_current_branch_stays_first_and_other_worktrees_are_not_switchable() {
        let options = matching_branches(
            &[
                branch("feature/held", false, Some("/tmp/other")),
                branch("main", true, Some("/tmp/current")),
                branch("feature/free", false, None),
            ],
            "",
        );

        assert_eq!(options[0].branch.name, "main");
        assert_eq!(options[0].availability, BranchAvailability::Current);
        assert_eq!(options[1].availability, BranchAvailability::Available);
        assert_eq!(
            options[2].availability,
            BranchAvailability::CheckedOutAt(PathBuf::from("/tmp/other"))
        );
    }

    #[test]
    fn branch_search_is_fuzzy_and_keeps_the_closest_match_first() {
        let options = matching_branches(
            &[
                branch("feature/usage-meter", false, None),
                branch("feature/user-menu", false, None),
            ],
            "usgmtr",
        );

        assert_eq!(options.len(), 1);
        assert_eq!(options[0].branch.name, "feature/usage-meter");
    }

    #[test]
    fn a_trimmed_name_is_offered_only_when_it_would_create_a_new_branch() {
        let branches = [branch("main", true, Some("/tmp/current"))];
        assert_eq!(
            new_branch_candidate(&branches, "  feature/new  "),
            Some("feature/new".into())
        );
        assert_eq!(new_branch_candidate(&branches, " main "), None);
        assert_eq!(new_branch_candidate(&branches, "  "), None);
    }

    #[test]
    fn return_prefers_a_fuzzy_existing_match_and_creates_only_without_one() {
        let branches = [
            branch("main", true, Some("/tmp/current")),
            branch("feature/usage-meter", false, None),
        ];
        assert_eq!(
            branch_submission(&branches, "usgmtr"),
            Some(BranchSubmission::Switch("feature/usage-meter".into()))
        );
        assert_eq!(
            branch_submission(&branches, "feature/new"),
            Some(BranchSubmission::Create("feature/new".into()))
        );
    }
}
