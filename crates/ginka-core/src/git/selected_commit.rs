//! Literal file selections committed through a private copy of the index.
//!
//! Git's partial commit updates the selected entries in that copy while
//! retaining every unrelated staged blob. The real index is locked for the
//! transaction and replaced only after Git and its hooks succeed.

use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};

use super::{Git, git_raw, head_commit};

/// Commit the complete worktree contents of explicitly selected files.
///
/// Paths are literal, workspace-relative files; directories and traversal
/// are refused. Selecting a staged rename also selects its old path. Other
/// staged entries are retained, including partial staging. Git hooks run
/// normally; a refused commit leaves the real index untouched. An existing
/// index lock or an in-progress merge is refused rather than overwritten.
pub fn commit_selected(worktree: &Path, message: &str, paths: &[String]) -> Result<String> {
    ensure!(!message.trim().is_empty(), "a commit needs a message");
    let index = index_path(worktree)?;
    let _lock = IndexLock::acquire(&index)?;
    let prepared = Selection::prepare(worktree, &index, paths)?;
    let mut args = vec![
        "--literal-pathspecs",
        "commit",
        "--only",
        "-m",
        message,
        "--",
    ];
    args.extend(prepared.paths.iter().map(String::as_str));
    prepared.run(worktree, &args)?;
    // Git has already moved HEAD. Retain the completed index if installation
    // fails, so an I/O error never discards the only recovery copy.
    if let Err(error) = fs::rename(&prepared.index, &index) {
        let recovery = prepared.directory.keep().join("index");
        bail!(
            "commit succeeded, but could not install the index: {error}; recovery index: {}",
            recovery.display()
        );
    }
    head_commit(worktree).context("committed, but git reports no HEAD")
}

/// Describe precisely the selected worktree contents for message generation.
///
/// Includes new files, rename deletions and repositories without HEAD. The
/// real index and worktree remain unchanged; ignored paths are refused by Git.
pub fn describe_selected(worktree: &Path, paths: &[String]) -> Result<(Vec<String>, String)> {
    let prepared = Selection::prepare(worktree, &index_path(worktree)?, paths)?;
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        "--cached",
        "--no-ext-diff",
        "--no-color",
        "--",
    ];
    args.extend(prepared.paths.iter().map(String::as_str));
    let diff = prepared.run(worktree, &args)?;
    ensure!(
        !diff.trim().is_empty(),
        "nothing to describe: selected files are unchanged"
    );
    Ok((prepared.paths, diff))
}

fn index_path(worktree: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(Git::new(worktree).run(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ])?))
}

struct IndexLock(PathBuf);

impl IndexLock {
    fn acquire(index: &Path) -> Result<Self> {
        let path = index.with_extension("lock");
        // The index has no extension: Git's lock is index.lock, not a sibling
        // of the common repository's index when this is a linked worktree.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "locking {}; another Git operation may be running",
                    path.display()
                )
            })?;
        Ok(Self(path))
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Selection {
    directory: tempfile::TempDir,
    index: PathBuf,
    paths: Vec<String>,
}

impl Selection {
    fn prepare(worktree: &Path, real_index: &Path, paths: &[String]) -> Result<Self> {
        ensure!(!paths.is_empty(), "select at least one file");
        let mut selected = BTreeSet::new();
        for path in paths {
            ensure!(
                !path.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|component| matches!(component, Component::Normal(_))),
                "selection must be a workspace-relative file: {path}"
            );
            selected.insert(path.clone());
        }
        // Porcelain -z puts a rename's destination first and then its source.
        // Never split or unquote names: spaces, newlines and wildcard syntax
        // are file names here, not Git pathspec instructions.
        let status = git_raw(
            worktree,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
            ],
        )?;
        let mut changed = BTreeSet::new();
        let mut records = status.split('\0');
        while let Some(record) = records.next() {
            if record.len() < 3 {
                continue;
            }
            changed.insert(&record[3..]);
            if record.as_bytes()[..2].contains(&b'R') || record.as_bytes()[..2].contains(&b'C') {
                let old = records.next().context("incomplete Git rename status")?;
                changed.insert(old);
                if record.as_bytes()[..2].contains(&b'R') && selected.contains(&record[3..]) {
                    selected.insert(old.to_string());
                }
            }
        }
        for path in &selected {
            match fs::symlink_metadata(worktree.join(path)) {
                Ok(meta) => ensure!(!meta.is_dir(), "select files, not a directory: {path}"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // A missing directory still expands recursively as a Git
                    // pathspec. Only an exact status path can name a deletion.
                    ensure!(
                        changed.contains(path.as_str()),
                        "selection must name a file, not a missing directory or path: {path}"
                    );
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("inspecting selected file {path}"));
                }
            }
        }
        let directory = tempfile::Builder::new()
            .prefix("ginka-selected-")
            .tempdir_in(real_index.parent().context("index has no parent")?)?;
        let index = directory.path().join("index");
        let selection = Self {
            directory,
            index,
            paths: selected.into_iter().collect(),
        };
        if real_index.exists() {
            fs::copy(real_index, &selection.index)
                .context("copying the index for selected files")?;
        } else {
            selection.run(worktree, &["read-tree", "--empty"])?;
        }
        // Restore selected entries to HEAD before adding their latest contents:
        // a deletion already staged no longer has an index entry for `add`.
        if head_commit(worktree).is_some() {
            let mut args = vec!["--literal-pathspecs", "reset", "--quiet", "HEAD", "--"];
            args.extend(selection.paths.iter().map(String::as_str));
            selection.run(worktree, &args)?;
        }
        let mut args = vec!["--literal-pathspecs", "add", "-A", "--"];
        args.extend(selection.paths.iter().map(String::as_str));
        selection.run(worktree, &args)?;
        Ok(selection)
    }

    fn run(&self, worktree: &Path, args: &[&str]) -> Result<String> {
        Git::new(worktree).run_with_env(args, &[("GIT_INDEX_FILE", self.index.as_os_str())])
    }
}
