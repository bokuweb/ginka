//! A pull request's checks, and handing the failing ones to an agent —
//! Orca's checks view and "Fix broken checks".
//!
//! Read with `gh pr checks --json`, the GitHub CLI Ginka already uses for
//! pull requests, so it holds no token. What a failure needs to be fixed is
//! its name and where its log is; the agent has `gh` too, and the prompt
//! says how to read the failing log rather than pasting megabytes of it.

use anyhow::{Context as _, Result};
use ginka_protocol::model::{CheckRun, CheckState};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

/// One row of `gh pr checks --json name,bucket,link,workflow`.
#[derive(Debug, Deserialize)]
struct GhCheck {
    name: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    workflow: String,
}

/// Read `gh pr checks --json name,bucket,link,workflow` output. An unknown
/// bucket is read as pending: a state this build has not met is not a
/// failure to act on, nor a pass to rely on.
pub fn parse(json: &str) -> Result<Vec<CheckRun>> {
    let rows: Vec<GhCheck> =
        serde_json::from_str(json.trim()).context("reading gh pr checks output")?;
    Ok(rows
        .into_iter()
        .map(|row| CheckRun {
            name: row.name,
            workflow: (!row.workflow.is_empty()).then_some(row.workflow),
            state: match row.bucket.as_str() {
                "pass" => CheckState::Passed,
                "fail" => CheckState::Failed,
                "cancel" => CheckState::Cancelled,
                "skipping" => CheckState::Skipped,
                _ => CheckState::Pending,
            },
            link: (!row.link.is_empty()).then_some(row.link),
        })
        .collect())
}

/// The checks of the pull request open from the worktree's branch.
///
/// `gh pr checks` exits non-zero both when it cannot find a pull request
/// and when some checks failed; only the first is an error here, told
/// apart by whether it printed the JSON it was asked for.
pub fn read(worktree: &Path) -> Result<Vec<CheckRun>> {
    let mut command = Command::new("gh");
    crate::tool_path::apply(&mut command);
    let output = command
        .current_dir(worktree)
        .args(["pr", "checks", "--json", "name,bucket,link,workflow"])
        .output()
        .context("running gh: the GitHub CLI is what reads a pull request's checks")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim_start().starts_with('[') {
        return parse(&stdout);
    }
    anyhow::bail!(
        "gh pr checks failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// The message that asks an agent to fix the failing checks, or `None` when
/// nothing failed — a request to fix green checks is not one to send.
pub fn fix_prompt(checks: &[CheckRun]) -> Option<String> {
    let failing: Vec<&CheckRun> = checks
        .iter()
        .filter(|check| check.state == CheckState::Failed)
        .collect();
    if failing.is_empty() {
        return None;
    }
    let mut text = format!(
        "{} check{} on this branch's pull request failed:\n\n",
        failing.len(),
        if failing.len() == 1 { "" } else { "s" }
    );
    for check in &failing {
        let name = match &check.workflow {
            Some(workflow) => format!("{workflow} / {}", check.name),
            None => check.name.clone(),
        };
        match &check.link {
            Some(link) => text.push_str(&format!("- {name} — {link}\n")),
            None => text.push_str(&format!("- {name}\n")),
        }
    }
    text.push_str(
        "\nRead each failure's log (`gh run view <run id> --log-failed`, the run id is in \
         its link), find the cause, and fix it in the code. Reproduce it locally first \
         where you can, and run the same check again before you finish. Do not disable, \
         skip or loosen the checks to make them pass.",
    );
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
      {"name":"test (ubuntu)","bucket":"fail","link":"https://github.com/o/r/actions/runs/1/job/2","workflow":"CI"},
      {"name":"clippy","bucket":"pass","link":"https://github.com/o/r/actions/runs/1/job/3","workflow":"CI"},
      {"name":"deploy-preview","bucket":"pending","link":"","workflow":""},
      {"name":"docs","bucket":"skipping","link":"","workflow":"Docs"},
      {"name":"lint","bucket":"something-new","link":"","workflow":""}
    ]"#;

    #[test]
    fn checks_are_read_with_their_state_workflow_and_log() {
        let checks = parse(SAMPLE).unwrap();
        assert_eq!(checks.len(), 5);
        assert_eq!(checks[0].state, CheckState::Failed);
        assert_eq!(checks[0].workflow.as_deref(), Some("CI"));
        assert_eq!(
            checks[0].link.as_deref(),
            Some("https://github.com/o/r/actions/runs/1/job/2")
        );
        assert_eq!(checks[1].state, CheckState::Passed);
        assert_eq!(checks[2].state, CheckState::Pending);
        assert_eq!(checks[2].link, None, "an empty link is no link");
        assert_eq!(checks[3].state, CheckState::Skipped);
        assert_eq!(
            checks[4].state,
            CheckState::Pending,
            "an unknown state is not a failure"
        );
        assert!(parse("not json").is_err());
    }

    #[test]
    fn only_failures_are_handed_over_and_green_is_nothing_to_fix() {
        let checks = parse(SAMPLE).unwrap();
        let prompt = fix_prompt(&checks).unwrap();
        assert!(prompt.starts_with("1 check on this branch's pull request failed:"));
        assert!(
            prompt.contains("- CI / test (ubuntu) — https://github.com/o/r/actions/runs/1/job/2")
        );
        assert!(!prompt.contains("clippy"));
        assert!(prompt.contains("--log-failed"));
        assert!(prompt.contains("Do not disable"));

        let green: Vec<_> = checks
            .into_iter()
            .filter(|check| check.state != CheckState::Failed)
            .collect();
        assert_eq!(fix_prompt(&green), None);
    }
}
