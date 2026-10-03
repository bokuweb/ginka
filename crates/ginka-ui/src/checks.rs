//! A pull request's checks as the Git surface lists them (Orca's checks
//! view): what needs the reader first, and one line that sums them up.

use ginka_protocol::model::{CheckRun, CheckState};

/// The order a state is listed in: what failed first, then what is still
/// running, then everything that needs nothing.
fn rank(state: CheckState) -> u8 {
    match state {
        CheckState::Failed => 0,
        CheckState::Cancelled => 1,
        CheckState::Pending => 2,
        CheckState::Passed => 3,
        CheckState::Skipped => 4,
    }
}

/// The checks in the order they are listed: by [`rank`], then by name, so
/// a refresh does not shuffle rows that did not change.
pub fn ordered(checks: &[CheckRun]) -> Vec<CheckRun> {
    let mut ordered = checks.to_vec();
    ordered.sort_by(|a, b| {
        rank(a.state)
            .cmp(&rank(b.state))
            .then_with(|| a.name.cmp(&b.name))
    });
    ordered
}

/// One line for the lot — "2 failed · 1 running · 5 passed" — leaving out
/// states no check is in. Skipped checks are not counted: they ran nothing.
/// `None` when there is nothing to sum up.
pub fn summary(checks: &[CheckRun]) -> Option<String> {
    let count = |state| checks.iter().filter(|check| check.state == state).count();
    let parts: Vec<String> = [
        (CheckState::Failed, "checks.count.failed"),
        (CheckState::Cancelled, "checks.count.cancelled"),
        (CheckState::Pending, "checks.count.running"),
        (CheckState::Passed, "checks.count.passed"),
    ]
    .into_iter()
    .filter_map(|(state, key)| match count(state) {
        0 => None,
        n => Some(rust_i18n::t!(key, n = n).to_string()),
    })
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// What a check's row says about its state, in a word as well as a mark, so
/// it is not told by colour alone (§6.4).
pub fn label(state: CheckState) -> String {
    let (mark, key) = match state {
        CheckState::Failed => ("✕", "checks.state.failed"),
        CheckState::Cancelled => ("⊘", "checks.state.cancelled"),
        CheckState::Pending => ("…", "checks.state.running"),
        CheckState::Passed => ("✓", "checks.state.passed"),
        CheckState::Skipped => ("–", "checks.state.skipped"),
    };
    format!("{mark} {}", rust_i18n::t!(key))
}

/// The row's name: the job, after its workflow when it has one.
pub fn title(check: &CheckRun) -> String {
    match check.workflow.as_deref().filter(|w| !w.is_empty()) {
        Some(workflow) => format!("{workflow} / {}", check.name),
        None => check.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, state: CheckState) -> CheckRun {
        CheckRun {
            name: name.to_string(),
            workflow: None,
            state,
            link: None,
        }
    }

    #[test]
    fn failures_come_first_then_running_then_the_rest_by_name() {
        let listed = ordered(&[
            check("lint", CheckState::Passed),
            check("docs", CheckState::Skipped),
            check("test (macos)", CheckState::Pending),
            check("test (linux)", CheckState::Failed),
            check("build", CheckState::Passed),
        ]);
        let names: Vec<&str> = listed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["test (linux)", "test (macos)", "build", "lint", "docs"]
        );
    }

    #[test]
    fn the_summary_counts_each_state_present_and_leaves_skips_out() {
        let checks = [
            check("a", CheckState::Failed),
            check("b", CheckState::Failed),
            check("c", CheckState::Pending),
            check("d", CheckState::Passed),
            check("e", CheckState::Skipped),
        ];
        assert_eq!(
            summary(&checks).as_deref(),
            Some("2 failed · 1 running · 1 passed")
        );
        assert_eq!(summary(&[check("e", CheckState::Skipped)]), None);
        assert_eq!(summary(&[]), None);
    }

    #[test]
    fn a_job_is_named_after_its_workflow_when_it_has_one() {
        let mut run = check("test", CheckState::Passed);
        assert_eq!(title(&run), "test");
        run.workflow = Some("CI".into());
        assert_eq!(title(&run), "CI / test");
    }
}
