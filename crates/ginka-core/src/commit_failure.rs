//! Handing a refused commit to an agent — Orca's "Fix with AI" on commit
//! failure.
//!
//! A pre-commit hook (a linter, a formatter, a test run) refusing a commit
//! says exactly what is wrong, in its own output. The prompt carries that
//! output, the files that were going in and the message that was going to
//! be used, and asks for the fix rather than for the hook to be skipped.

use anyhow::{Context as _, Result};
use std::path::Path;
use std::process::Command;

/// Most lines of the failure kept: the end of a hook's output is where it
/// says what failed, and a full test log is the agent's to re-run.
pub const MAX_OUTPUT_LINES: usize = 80;

/// Most staged paths named; the rest are counted.
pub const MAX_PATHS: usize = 50;

/// The paths staged for the next commit, as git lists them.
pub fn staged_paths(worktree: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--cached", "--name-only", "-z"])
        .output()
        .context("listing the staged files")?;
    anyhow::ensure!(
        output.status.success(),
        "git diff --cached failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// The message that asks an agent to make a refused commit go through.
pub fn prompt(message: &str, output: &str, staged: &[String]) -> String {
    let mut text = String::from("A commit in this worktree was refused");
    if staged.is_empty() {
        text.push_str(".\n");
    } else {
        text.push_str(&format!(
            ". It was going to include {} file{}:\n\n",
            staged.len(),
            if staged.len() == 1 { "" } else { "s" }
        ));
        for path in staged.iter().take(MAX_PATHS) {
            text.push_str(&format!("- {path}\n"));
        }
        if staged.len() > MAX_PATHS {
            text.push_str(&format!("- …and {} more\n", staged.len() - MAX_PATHS));
        }
    }
    let message = message.trim();
    if !message.is_empty() {
        text.push_str(&format!("\nIts message was:\n\n{message}\n"));
    }
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let kept = &lines[lines.len().saturating_sub(MAX_OUTPUT_LINES)..];
    text.push_str("\nWhat git and its hooks said:\n\n```\n");
    if lines.len() > kept.len() {
        text.push_str(&format!(
            "… ({} earlier lines left out)\n",
            lines.len() - kept.len()
        ));
    }
    text.push_str(&kept.join("\n"));
    text.push_str("\n```\n\n");
    text.push_str(
        "Fix what it reports in the files themselves, and `git add` your fixes. \
         Do not bypass the hooks (no `--no-verify`) and do not commit — the \
         commit will be made again once you are done.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_carries_the_files_the_message_and_the_end_of_the_output() {
        let output = (1..=100)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\nerror: trailing whitespace in src/a.rs";
        let text = prompt("Add the parser", &output, &["src/a.rs".into()]);
        assert!(text.contains("It was going to include 1 file:\n\n- src/a.rs\n"));
        assert!(text.contains("Its message was:\n\nAdd the parser"));
        assert!(text.contains("error: trailing whitespace in src/a.rs"));
        assert!(
            !text.contains("line 1\n"),
            "the start of a long log is left out"
        );
        assert!(text.contains("(21 earlier lines left out)"));
        assert!(text.contains("no `--no-verify`"));
        assert!(text.contains("do not commit"));
    }

    #[test]
    fn a_failure_with_nothing_staged_still_reads_as_a_sentence() {
        let text = prompt("", "nothing to commit", &[]);
        assert!(text.starts_with("A commit in this worktree was refused.\n"));
        assert!(!text.contains("Its message was"));
    }
}
