//! `.worktreeinclude`: the ignored files a new worktree should start with.
//!
//! Orca's idea. A worktree is a fresh checkout, so everything git ignores —
//! `.env`, a local config, a downloaded fixture — is missing from it, and an
//! agent started there fails on the first thing that needed one. A repository
//! lists those files in `.worktreeinclude` at its root, one pattern a line in
//! gitignore's shape, and each new workspace gets a copy of the ones that
//! exist.
//!
//! Only ignored files are candidates: a tracked file is in the checkout
//! already, and copying an untracked-but-not-ignored one would hand the agent
//! someone's half-finished work as if it were the branch's.

use anyhow::{Context as _, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;
use std::process::Command;

/// The file the patterns are read from, at the repository root.
pub const FILE_NAME: &str = ".worktreeinclude";

/// How many files one worktree is given at most: past this, the patterns are
/// matching a build directory, not configuration.
pub const MAX_FILES: usize = 2_000;

/// Read the patterns: one a line, blank lines and `#` comments skipped.
pub fn patterns(repo: &Path) -> Vec<String> {
    std::fs::read_to_string(repo.join(FILE_NAME))
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// The patterns in `.worktreeinclude`'s text.
pub fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Compile patterns the way gitignore reads them.
///
/// A pattern with no `/` matches a name at any depth (`.env` is every
/// `.env`); one with a `/` is anchored at the root; a trailing `/` means
/// everything under that directory.
pub fn matcher(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = pattern.trim_start_matches("./");
        // A trailing slash says "a directory"; its contents are matched below
        // either way, so it only has to be taken off.
        let pattern = pattern.strip_suffix('/').unwrap_or(pattern);
        let anchored = pattern.contains('/');
        let pattern = pattern.trim_start_matches('/');
        let base = if anchored {
            pattern.to_string()
        } else {
            format!("**/{pattern}")
        };
        builder.add(Glob::new(&base).with_context(|| format!("bad pattern {pattern:?}"))?);
        // A directory pattern takes what is inside it; so does a plain one
        // that happens to name a directory, as it would in gitignore.
        builder.add(Glob::new(&format!("{base}/**"))?);
    }
    Ok(builder.build()?)
}

/// Copy the included files from `repo` into a new `worktree`, answering with
/// the paths copied. A file the worktree already has is left alone.
pub fn copy_included(repo: &Path, worktree: &Path) -> Result<Vec<String>> {
    let patterns = patterns(repo);
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let matcher = matcher(&patterns)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .context("listing the repository's ignored files")?;
    anyhow::ensure!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut copied = Vec::new();
    for path in String::from_utf8_lossy(&output.stdout).split('\0') {
        if path.is_empty() || !matcher.is_match(path) {
            continue;
        }
        if copied.len() >= MAX_FILES {
            tracing::warn!(limit = MAX_FILES, "stopped copying .worktreeinclude files");
            break;
        }
        let source = repo.join(path);
        let target = worktree.join(path);
        // A link is copied as what it points at only when that is a file;
        // anything else — a socket, a directory link — is not configuration.
        if !source.is_file() || target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source, &target).with_context(|| format!("copying {path}"))?;
        copied.push(path.to_string());
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn comments_and_blank_lines_are_not_patterns() {
        assert_eq!(
            parse("# env\n\n.env\n  config/local.toml  \n"),
            vec![".env", "config/local.toml"]
        );
    }

    #[test]
    fn patterns_read_the_way_gitignore_reads_them() {
        let set = matcher(&[
            ".env".into(),
            "/config/local.toml".into(),
            "fixtures/".into(),
        ])
        .unwrap();
        assert!(set.is_match(".env"));
        assert!(
            set.is_match("apps/web/.env"),
            "a bare name matches at any depth"
        );
        assert!(set.is_match("config/local.toml"));
        assert!(
            !set.is_match("apps/config/local.toml"),
            "a slash anchors it"
        );
        assert!(
            set.is_match("fixtures/big/one.json"),
            "a directory takes its contents"
        );
        assert!(!set.is_match(".env.example"));
    }

    #[test]
    fn a_new_worktree_gets_the_ignored_files_it_lists_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@ginka.invalid"]);
        git(&repo, &["config", "user.name", "T"]);
        std::fs::write(repo.join(".gitignore"), ".env\nsecret.txt\ntarget/\n").unwrap();
        std::fs::write(repo.join(FILE_NAME), ".env\n").unwrap();
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "first"],
        );
        std::fs::write(repo.join(".env"), "KEY=1\n").unwrap();
        std::fs::write(repo.join("secret.txt"), "not listed\n").unwrap();
        std::fs::create_dir_all(repo.join("target")).unwrap();
        std::fs::write(repo.join("target/out"), "build\n").unwrap();
        std::fs::write(repo.join("draft.md"), "untracked, not ignored\n").unwrap();

        let worktree = dir.path().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        let copied = copy_included(&repo, &worktree).unwrap();
        assert_eq!(copied, vec![".env"]);
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env")).unwrap(),
            "KEY=1\n"
        );
        assert!(!worktree.join("secret.txt").exists(), "only what is listed");
        assert!(!worktree.join("draft.md").exists(), "only what is ignored");

        // Run again: a file the worktree already has is left as it is.
        std::fs::write(worktree.join(".env"), "KEY=edited\n").unwrap();
        assert!(copy_included(&repo, &worktree).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env")).unwrap(),
            "KEY=edited\n"
        );
    }

    #[test]
    fn a_repository_without_the_file_copies_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(copy_included(dir.path(), dir.path()).unwrap().is_empty());
    }
}
