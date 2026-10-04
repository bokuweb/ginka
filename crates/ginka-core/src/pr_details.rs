//! A pull request's title and description, written by an agent — Orca's
//! "Generate pull request details with AI".
//!
//! `gh pr create --fill` takes the first commit's subject and every commit's
//! body, which for an agent's branch is a list of "wip" and "address review".
//! The model is given what a reviewer needs — the branch's commit subjects,
//! the files it touches and a capped diff against its base — and asked for a
//! title and a body in a fixed shape that is parsed back here.

use anyhow::{Context as _, Result, bail};
use std::path::Path;

/// Longest title kept; GitHub shows about this much in a list.
pub const MAX_TITLE_CHARS: usize = 100;

/// Longest body kept, in characters: a description, not a changelog.
pub const MAX_BODY_CHARS: usize = 6_000;

/// What `gh pr create` is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestDetails {
    /// One line, at most [`MAX_TITLE_CHARS`].
    pub title: String,
    /// Markdown, at most [`MAX_BODY_CHARS`].
    pub body: String,
}

/// The prompt the details are written from. The diff is capped at
/// [`crate::commit::MAX_DIFF_BYTES`] and the cut is said out loud.
pub fn build_prompt(commits: &[String], files: &[String], diff: &str) -> String {
    let mut prompt = String::from(
        "Write a GitHub pull request title and description for the branch below.\n\
         Answer in exactly this shape and nothing else:\n\n\
         TITLE: <one line, imperative mood, under 72 characters>\n\
         BODY:\n<markdown: what changed and why, for a reviewer; short sections, \
         no heading for the title>\n\n\
         Commits on the branch:\n",
    );
    for commit in commits {
        prompt.push_str(&format!("- {commit}\n"));
    }
    prompt.push_str("\nFiles changed:\n");
    for file in files {
        prompt.push_str(&format!("- {file}\n"));
    }
    prompt.push_str("\nDiff:\n");
    let cap = crate::commit::MAX_DIFF_BYTES;
    if diff.len() > cap {
        let mut cut = cap;
        while cut > 0 && !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        prompt.push_str(&diff[..cut]);
        prompt.push_str("\n… diff truncated here; the commit and file lists above are complete.\n");
    } else {
        prompt.push_str(diff);
    }
    prompt
}

/// What the branch at `worktree` did since it left `base`: its commit
/// subjects (oldest first, at most 100), the files it changed, and the diff.
pub fn describe(worktree: &Path, base: &str) -> Result<(Vec<String>, Vec<String>, String)> {
    let fork = crate::git::fork_point(worktree, base)?;
    let run = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        anyhow::ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let range = format!("{fork}..HEAD");
    let commits: Vec<String> =
        run(&["log", "--reverse", "--max-count=100", "--format=%s", &range])?
            .lines()
            .map(str::to_string)
            .collect();
    anyhow::ensure!(
        !commits.is_empty(),
        "the branch has no commits since {base}"
    );
    let files = run(&["diff", "--name-only", &range])?
        .lines()
        .map(str::to_string)
        .collect();
    let diff = run(&["diff", "--no-color", "--no-ext-diff", &range])?;
    Ok((commits, files, diff))
}

/// Have `driver` write the details for the branch at `worktree`.
pub fn generate(
    driver: &dyn crate::driver::AgentDriver,
    worktree: &Path,
    env: &[(String, String)],
    base: &str,
) -> Result<PullRequestDetails> {
    let (commits, files, diff) = describe(worktree, base)?;
    let said = crate::commit::ask_cheap(
        driver,
        worktree,
        env,
        &build_prompt(&commits, &files, &diff),
    )?;
    PullRequestDetails::parse(&said)
}

impl PullRequestDetails {
    /// Read a model's answer. Tolerates a code fence around it and a
    /// `Title:` in any case; refuses an answer with no title, because a pull
    /// request opened with an empty one is worse than `--fill`.
    pub fn parse(raw: &str) -> Result<Self> {
        let text = raw.trim();
        let text = text
            .strip_prefix("```")
            .map(|rest| rest.split_once('\n').map_or("", |(_, body)| body))
            .and_then(|rest| rest.trim_end().strip_suffix("```"))
            .unwrap_or(text);
        let mut title = None;
        let mut body = Vec::new();
        let mut in_body = false;
        for line in text.lines() {
            let trimmed = line.trim_start();
            let upper = trimmed.to_ascii_uppercase();
            if !in_body && upper.starts_with("TITLE:") {
                title = Some(trimmed["TITLE:".len()..].trim().to_string());
            } else if !in_body && upper.starts_with("BODY:") {
                in_body = true;
                let rest = trimmed["BODY:".len()..].trim();
                if !rest.is_empty() {
                    body.push(rest.to_string());
                }
            } else if in_body {
                body.push(line.to_string());
            }
        }
        let title = title
            .map(|title| title.trim_matches(['"', '\'', '`']).trim().to_string())
            .filter(|title| !title.is_empty());
        let Some(title) = title else {
            bail!("the model wrote no pull request title");
        };
        Ok(Self {
            title: bounded(&title, MAX_TITLE_CHARS),
            body: bounded(body.join("\n").trim(), MAX_BODY_CHARS),
        })
    }
}

fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_in_the_asked_shape_becomes_a_title_and_a_body() {
        let details = PullRequestDetails::parse(
            "TITLE: Add a branch diff source\nBODY:\n## What\nA diff against the base.\n\n## Why\nReview.",
        )
        .unwrap();
        assert_eq!(details.title, "Add a branch diff source");
        assert_eq!(
            details.body,
            "## What\nA diff against the base.\n\n## Why\nReview."
        );
    }

    #[test]
    fn fences_quotes_and_case_are_forgiven_but_a_missing_title_is_not() {
        let details = PullRequestDetails::parse(
            "```markdown\nTitle: \"Fix the parser\"\nBody: One line.\n```",
        )
        .unwrap();
        assert_eq!(details.title, "Fix the parser");
        assert_eq!(details.body, "One line.");
        assert!(PullRequestDetails::parse("Here is a description of the change.").is_err());
        assert!(PullRequestDetails::parse("TITLE:\nBODY: text").is_err());
    }

    #[test]
    fn a_long_title_and_body_are_cut_and_marked() {
        let raw = format!("TITLE: {}\nBODY:\n{}", "t".repeat(300), "b".repeat(9_000));
        let details = PullRequestDetails::parse(&raw).unwrap();
        assert_eq!(details.title.chars().count(), MAX_TITLE_CHARS);
        assert!(details.title.ends_with('…'));
        assert_eq!(details.body.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn the_prompt_lists_commits_and_files_and_says_when_the_diff_was_cut() {
        let big = "+x\n".repeat(crate::commit::MAX_DIFF_BYTES);
        let prompt = build_prompt(&["Add the parser".into()], &["src/parser.rs".into()], &big);
        assert!(prompt.contains("- Add the parser\n"));
        assert!(prompt.contains("- src/parser.rs\n"));
        assert!(prompt.contains("TITLE:"));
        assert!(prompt.contains("diff truncated here"));
        assert!(prompt.len() < big.len() + 2_000);
    }
}
