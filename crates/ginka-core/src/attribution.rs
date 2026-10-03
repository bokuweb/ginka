//! Which added lines an agent wrote — Orca's line-level AI attribution.
//!
//! Every completed turn left two snapshots (§3.3 N8): the worktree as the
//! agent was handed it and as it ended. The lines a turn added are its own
//! work. A line added in the current diff whose text is one of those is
//! marked as the agent's; anything else — a line a person typed, or an
//! agent's line a person has since edited — is a person's. Matching is by
//! text and counted, so two identical lines are only both the agent's if it
//! wrote both.

use anyhow::Result;
use ginka_protocol::model::{FileChange, LineKind};
use std::collections::HashMap;
use std::path::Path;

/// The lines each turn added, by file, across `turns` — pairs of a turn's
/// start and end snapshot commits.
pub fn turn_added_lines(
    worktree: &Path,
    turns: &[(String, String)],
) -> Result<HashMap<String, Vec<String>>> {
    let mut added: HashMap<String, Vec<String>> = HashMap::new();
    for (start, end) in turns {
        for file in crate::git::changes_between_with_context(worktree, start, end, 0)? {
            let lines = added.entry(file.path.clone()).or_default();
            for hunk in &file.hunks {
                lines.extend(
                    hunk.lines
                        .iter()
                        .filter(|line| line.kind == LineKind::Added)
                        .map(|line| line.text.trim_end().to_string()),
                );
            }
        }
    }
    Ok(added)
}

/// Mark every added line in `files` as the agent's or a person's, from the
/// lines turns added (`turn_added_lines`). Context and removed lines are
/// left unmarked.
pub fn mark(files: &mut [FileChange], by_agent: &HashMap<String, Vec<String>>) {
    for file in files {
        let mut left: HashMap<&str, usize> = HashMap::new();
        if let Some(lines) = by_agent.get(&file.path) {
            for line in lines {
                *left.entry(line.as_str()).or_default() += 1;
            }
        }
        for hunk in &mut file.hunks {
            for line in &mut hunk.lines {
                if line.kind != LineKind::Added {
                    continue;
                }
                let text = line.text.trim_end();
                let agent = match left.get_mut(text) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        true
                    }
                    _ => false,
                };
                line.by_agent = Some(agent);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.email=t@ginka.invalid",
                "-c",
                "user.name=T",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A snapshot of the worktree as a commit on no branch, the way a
    /// checkpoint takes one.
    fn snapshot(dir: &Path) -> String {
        git(dir, &["add", "-A"]);
        let tree = git(dir, &["write-tree"]);
        let commit = git(dir, &["commit-tree", &tree, "-p", "HEAD", "-m", "snapshot"]);
        git(dir, &["reset", "-q"]);
        commit
    }

    #[test]
    fn what_a_turn_added_is_the_agents_until_a_person_edits_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "base"]);

        // A turn: the agent adds two functions.
        let start = snapshot(repo);
        std::fs::write(repo.join("lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let end = snapshot(repo);
        // Afterwards a person edits one of them and adds a line of their own.
        std::fs::write(
            repo.join("lib.rs"),
            "fn a() {}\nfn b() {}\nfn c(x: u8) {}\n// mine\n",
        )
        .unwrap();

        let by_agent = turn_added_lines(repo, &[(start, end)]).unwrap();
        let mut files = crate::git::changes_with_context(
            repo,
            &ginka_protocol::model::ChangeSource::Uncommitted,
            3,
        )
        .unwrap();
        mark(&mut files, &by_agent);
        let marked: Vec<(String, Option<bool>)> = files[0]
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .filter(|line| line.kind == LineKind::Added)
            .map(|line| (line.text.clone(), line.by_agent))
            .collect();
        assert_eq!(
            marked,
            vec![
                ("fn b() {}".to_string(), Some(true)),
                ("fn c(x: u8) {}".to_string(), Some(false)),
                ("// mine".to_string(), Some(false)),
            ]
        );
    }

    #[test]
    fn identical_lines_are_only_the_agents_as_many_times_as_it_wrote_them() {
        let mut by_agent = HashMap::new();
        by_agent.insert("a.rs".to_string(), vec!["}".to_string()]);
        let line = |text: &str| ginka_protocol::model::DiffLine {
            kind: LineKind::Added,
            text: text.into(),
            old_line: None,
            new_line: Some(1),
            words: Vec::new(),
            by_agent: None,
        };
        let mut files = vec![FileChange {
            path: "a.rs".into(),
            old_path: None,
            kind: ginka_protocol::model::ChangeKind::Modified,
            added: 2,
            removed: 0,
            binary: false,
            hunks: vec![ginka_protocol::model::Hunk {
                header: "@@ -0,0 +1,2 @@".into(),
                lines: vec![line("}"), line("}")],
            }],
        }];
        mark(&mut files, &by_agent);
        let marks: Vec<_> = files[0].hunks[0]
            .lines
            .iter()
            .map(|line| line.by_agent)
            .collect();
        assert_eq!(marks, vec![Some(true), Some(false)]);
    }
}
