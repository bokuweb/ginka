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
//!
//! `.worktreeshare` is the other half, Orca's shared directories: the heavy
//! ignored directories — `node_modules`, `.venv`, a model cache — that every
//! worktree would otherwise install or copy again. Each line names one
//! directory from the repository root, and a new worktree gets a symbolic
//! link to the source checkout's copy instead of one of its own. Fan-out's
//! five attempts then cost one install, not five; the price is that they
//! share it, so a directory an agent is expected to change does not belong
//! in the file.

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

/// The file the shared directories are read from, at the repository root.
pub const SHARE_FILE: &str = ".worktreeshare";

/// The comment written above the lines [`link_shared`] adds to git's exclude
/// file, so a reader of that file knows where they came from.
const EXCLUDE_MARK: &str = "# linked into worktrees by Ginka (.worktreeshare)";

/// Link the directories `.worktreeshare` lists from `repo` into a new
/// `worktree`, answering with the paths linked.
///
/// A line is a plain path from the root — no globs, since a link is one
/// decision about one directory — and it is taken only when `repo` has that
/// directory, git ignores it there, and the worktree has nothing at the path
/// yet. git ignores a *link* by a pattern without a trailing slash only
/// (`node_modules/` names directories, and a link is not one), so a link git
/// would show as a new file is excluded through the repository's
/// `info/exclude` — shared by every worktree, and a no-op for the source
/// checkout, which ignores the directory already.
pub fn link_shared(repo: &Path, worktree: &Path) -> Result<Vec<String>> {
    let listed = std::fs::read_to_string(repo.join(SHARE_FILE))
        .map(|text| parse(&text))
        .unwrap_or_default();
    let mut linked = Vec::new();
    for line in listed {
        let path = line.trim_start_matches("./").trim_matches('/').to_string();
        let safe = !path.is_empty()
            && Path::new(&path)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)));
        if !safe {
            tracing::warn!(
                line,
                "a .worktreeshare line must be a path inside the repository"
            );
            continue;
        }
        let source = repo.join(&path);
        let target = worktree.join(&path);
        if !source.is_dir() || target.symlink_metadata().is_ok() {
            continue;
        }
        if !ignored(repo, &path)? {
            tracing::warn!(path, "not linking a directory git does not ignore");
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        symlink_dir(&source, &target).with_context(|| format!("linking {path}"))?;
        if !ignored(worktree, &path)? {
            exclude(worktree, &path)?;
        }
        linked.push(path);
    }
    Ok(linked)
}

/// Whether git ignores `path` in the checkout at `dir`.
fn ignored(dir: &Path, path: &str) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q", "--", path])
        .status()
        .context("asking git whether a path is ignored")?;
    // 0 ignored, 1 not ignored, anything else is git failing.
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!("git check-ignore failed in {}", dir.display()),
    }
}

/// Add an anchored `path` to the repository's shared `info/exclude`.
fn exclude(worktree: &Path, path: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .context("finding the repository's git directory")?;
    anyhow::ensure!(
        output.status.success(),
        "git rev-parse --git-common-dir failed"
    );
    let common = std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let file = common.join("info").join("exclude");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    let entry = format!("/{path}");
    if existing.lines().any(|line| line.trim() == entry) {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.lines().any(|line| line == EXCLUDE_MARK) {
        text.push_str(EXCLUDE_MARK);
        text.push('\n');
    }
    text.push_str(&entry);
    text.push('\n');
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))
}

#[cfg(unix)]
fn symlink_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn symlink_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(source, target)
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

    #[test]
    fn a_shared_directory_is_linked_not_copied_and_stays_out_of_the_status() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@ginka.invalid"]);
        git(&repo, &["config", "user.name", "T"]);
        // The usual spelling, with the slash that does not match a link.
        std::fs::write(repo.join(".gitignore"), "node_modules/\n.venv/\n").unwrap();
        std::fs::write(
            repo.join(SHARE_FILE),
            "node_modules\n.venv\nsrc\n../outside\nmissing\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "first"],
        );
        std::fs::create_dir_all(repo.join("node_modules/left-pad")).unwrap();
        std::fs::write(repo.join("node_modules/left-pad/index.js"), "x\n").unwrap();

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
        let linked = link_shared(&repo, &worktree).unwrap();
        assert_eq!(
            linked,
            vec!["node_modules"],
            "not .venv (absent), not src (tracked), not a path out of the repository"
        );
        let link = worktree.join("node_modules");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(link.join("left-pad/index.js").is_file());

        let status = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&status.stdout),
            "",
            "the link is excluded, so the worktree reads as clean"
        );

        // A second worktree reuses the exclusion instead of repeating it.
        let second = dir.path().join("wt2");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "second",
                second.to_str().unwrap(),
            ],
        );
        assert_eq!(link_shared(&repo, &second).unwrap(), vec!["node_modules"]);
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap_or_default();
        assert_eq!(exclude.matches("/node_modules").count(), 1, "{exclude}");

        // Removing a worktree takes the link, never what it points at.
        git(
            &repo,
            &["worktree", "remove", "--force", second.to_str().unwrap()],
        );
        assert!(repo.join("node_modules/left-pad/index.js").is_file());
    }
}
