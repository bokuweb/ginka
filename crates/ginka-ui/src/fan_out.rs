//! Fan-out: one prompt tried several ways, and the attempts compared.
//!
//! Orca's race, made first-class: the same question in a worktree per
//! attempt, then the attempts side by side so the winner is kept and the rest
//! put away. The daemon does the work (`FanOut`); this module decides what a
//! fan-out is called, which agents it asks, and which workspaces belong to
//! the same one.

use crate::workspace::SessionRow;
use ginka_protocol::ids::slugify;
use ginka_protocol::rpc::Attempt;

/// The most attempts one fan-out asks for: past this the comparison is a
/// wall, not a choice.
pub const MAX_ATTEMPTS: usize = 6;

/// What the branches of a fan-out are called, from its prompt: its first
/// few words, then `-1`, `-2`, … per attempt. `try` when the prompt has no
/// words to take.
pub fn branch_prefix(prompt: &str) -> String {
    let words = prompt
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ");
    let slug = slugify(&words);
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "try".to_string()
    } else {
        format!("try-{slug}")
    }
}

/// The attempts for a fan-out: each agent as many times as it was asked for,
/// in the order given, capped at [`MAX_ATTEMPTS`].
pub fn attempts(counts: &[(String, usize)]) -> Vec<Attempt> {
    counts
        .iter()
        .flat_map(|(agent, count)| {
            std::iter::repeat_n(
                Attempt {
                    agent: agent.clone(),
                    model: None,
                    account: None,
                },
                *count,
            )
        })
        .take(MAX_ATTEMPTS)
        .collect()
}

/// The `<prefix>` of a fan-out branch named `<prefix>-<n>`, if it is one.
fn prefix_of(branch: &str) -> Option<&str> {
    let (prefix, number) = branch.rsplit_once('-')?;
    (!prefix.is_empty() && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .then_some(prefix)
}

/// The attempts `row` was one of: every live workspace in its project whose
/// branch is the same `<prefix>-<n>`, in attempt order. Empty unless there
/// are at least two, which is what makes it a comparison.
pub fn siblings<'a>(rows: &'a [SessionRow], row: &SessionRow) -> Vec<&'a SessionRow> {
    let Some(prefix) = prefix_of(&row.branch) else {
        return Vec::new();
    };
    let mut found: Vec<&SessionRow> = rows
        .iter()
        .filter(|other| !other.archived && other.origin == row.origin)
        .filter(|other| prefix_of(&other.branch) == Some(prefix))
        .collect();
    if found.len() < 2 {
        return Vec::new();
    }
    let number = |row: &SessionRow| {
        row.branch
            .rsplit_once('-')
            .and_then(|(_, n)| n.parse::<u32>().ok())
            .unwrap_or(0)
    };
    found.sort_by_key(|row| number(row));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fan_out_is_named_after_its_prompt() {
        assert_eq!(
            branch_prefix("Speed up the fixture CI please now"),
            "try-speed-up-the-fixture-ci"
        );
        assert_eq!(branch_prefix("   "), "try");
        assert!(branch_prefix(&"word ".repeat(40)).len() <= 44);
    }

    #[test]
    fn attempts_repeat_an_agent_and_stop_at_the_cap() {
        let asked = attempts(&[("claude".into(), 2), ("codex".into(), 1)]);
        assert_eq!(
            asked.iter().map(|a| a.agent.as_str()).collect::<Vec<_>>(),
            vec!["claude", "claude", "codex"]
        );
        assert_eq!(attempts(&[("claude".into(), 10)]).len(), MAX_ATTEMPTS);
        assert!(attempts(&[("claude".into(), 0)]).is_empty());
    }

    #[test]
    fn siblings_share_a_prefix_in_one_project_and_come_in_attempt_order() {
        let mut rows = SessionRow::samples();
        for row in &mut rows {
            row.archived = false;
            row.origin = "comet".into();
        }
        rows[0].branch = "try-fix-10".into();
        rows[1].branch = "try-fix-2".into();
        rows[2].branch = "try-fix-1".into();
        rows[3].branch = "main".into();
        let found = siblings(&rows, &rows[0]);
        assert_eq!(
            found
                .iter()
                .map(|row| row.branch.as_ref())
                .collect::<Vec<_>>(),
            vec!["try-fix-1", "try-fix-2", "try-fix-10"]
        );
        assert!(siblings(&rows, &rows[3]).is_empty(), "not a fan-out branch");

        rows[1].origin = "other".into();
        rows[2].archived = true;
        assert!(
            siblings(&rows, &rows[0]).is_empty(),
            "one left is not a comparison"
        );
    }
}
