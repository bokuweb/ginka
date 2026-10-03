//! Both sides of a changed image — Orca's image diff.
//!
//! git shows a changed PNG as "binary files differ", which is true and
//! useless to a reviewer. This reads the image as it was and as it is, from
//! the same revisions the diff source compares, and passes each through
//! [`crate::files::preview_image`]: only a recognised format within the
//! preview limit is sent, decided by the bytes, never by the extension.

use anyhow::{Context as _, Result, bail};
use ginka_protocol::model::{ChangeSource, FileImage};
use std::path::Path;
use std::process::Command;

/// Where one side of a change is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Side {
    /// A git object: `HEAD:path`, `:path` (the index), `<commit>:path`.
    Object(String),
    /// The file in the worktree.
    Worktree,
}

/// The image before and after `path` changed under `source`. A side that
/// does not exist (an added or deleted file) or is not a previewable image
/// is `None`. Sources measured from a checkpoint or a base name commits the
/// caller has to look up first; it resolves them and calls
/// [`sides_between`] instead.
pub fn sides(
    worktree: &Path,
    source: &ChangeSource,
    path: &str,
    old_path: Option<&str>,
) -> Result<(Option<FileImage>, Option<FileImage>)> {
    let safe = |path: &str| {
        !path.is_empty()
            && !path.starts_with('-')
            && Path::new(path)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
    };
    let before = old_path.unwrap_or(path);
    if !safe(path) || !safe(before) {
        bail!("{path:?} is not a path inside the worktree");
    }
    let (old, new) = match source {
        ChangeSource::Uncommitted => (Side::Object(format!("HEAD:{before}")), Side::Worktree),
        ChangeSource::Staged => (
            Side::Object(format!("HEAD:{before}")),
            Side::Object(format!(":{path}")),
        ),
        ChangeSource::Unstaged => (Side::Object(format!(":{before}")), Side::Worktree),
        ChangeSource::Commit { commit } => {
            anyhow::ensure!(
                crate::git::is_object_name(commit),
                "{commit:?} is not a commit id"
            );
            (
                Side::Object(format!("{commit}^:{before}")),
                Side::Object(format!("{commit}:{path}")),
            )
        }
        other => bail!("an image diff is not available for {other:?} yet"),
    };
    Ok((read(worktree, &old, before)?, read(worktree, &new, path)?))
}

/// The image at `from` and at `to` — a commit each, or the worktree when
/// `to` is `None`: what a turn changed (its start and end snapshots), or
/// what has happened since a checkpoint or a branch's fork point. Both ids
/// must be object names.
pub fn sides_between(
    worktree: &Path,
    from: &str,
    to: Option<&str>,
    path: &str,
    old_path: Option<&str>,
) -> Result<(Option<FileImage>, Option<FileImage>)> {
    let before = old_path.unwrap_or(path);
    if !safe_path(path) || !safe_path(before) {
        bail!("{path:?} is not a path inside the worktree");
    }
    for id in std::iter::once(from).chain(to) {
        anyhow::ensure!(crate::git::is_object_name(id), "{id:?} is not a commit id");
    }
    let new = match to {
        Some(to) => Side::Object(format!("{to}:{path}")),
        None => Side::Worktree,
    };
    Ok((
        read(worktree, &Side::Object(format!("{from}:{before}")), before)?,
        read(worktree, &new, path)?,
    ))
}

/// A path inside the worktree that cannot be read as an option.
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('-')
        && Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn read(worktree: &Path, side: &Side, path: &str) -> Result<Option<FileImage>> {
    match side {
        Side::Worktree => {
            let full = worktree.join(path);
            if !full.is_file() {
                return Ok(None);
            }
            let size = std::fs::metadata(&full)?.len();
            if size > crate::files::IMAGE_PREVIEW_LIMIT as u64 {
                return Ok(None);
            }
            Ok(crate::files::preview_image(&std::fs::read(&full)?))
        }
        Side::Object(object) => {
            let git = |args: &[&str]| {
                Command::new("git")
                    .arg("-C")
                    .arg(worktree)
                    .args(args)
                    .output()
                    .context("running git")
            };
            // Missing on this side (added, deleted, or no commits yet).
            let size = git(&["cat-file", "-s", object])?;
            if !size.status.success() {
                return Ok(None);
            }
            let size: u64 = String::from_utf8_lossy(&size.stdout)
                .trim()
                .parse()
                .unwrap_or(u64::MAX);
            if size > crate::files::IMAGE_PREVIEW_LIMIT as u64 {
                return Ok(None);
            }
            let blob = git(&["cat-file", "blob", object])?;
            if !blob.status.success() {
                return Ok(None);
            }
            Ok(crate::files::preview_image(&blob.stdout))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    fn git(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=t@ginka.invalid",
                    "-c",
                    "user.name=T",
                    "-c",
                    "commit.gpgsign=false"
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success(),
            "git {args:?}"
        );
    }

    fn png(tail: &[u8]) -> Vec<u8> {
        [PNG, tail].concat()
    }

    #[test]
    fn a_changed_image_is_read_as_it_was_and_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("logo.png"), png(b"old")).unwrap();
        std::fs::write(repo.join("blob.bin"), b"\x00\x01not an image").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "first"]);
        std::fs::write(repo.join("logo.png"), png(b"new")).unwrap();
        std::fs::write(repo.join("added.png"), png(b"fresh")).unwrap();

        let (old, new) = sides(repo, &ChangeSource::Uncommitted, "logo.png", None).unwrap();
        let (old, new) = (old.unwrap(), new.unwrap());
        assert_eq!(old.media_type, "image/png");
        assert_ne!(old.data_base64, new.data_base64);

        let (old, new) = sides(repo, &ChangeSource::Uncommitted, "added.png", None).unwrap();
        assert!(
            old.is_none() && new.is_some(),
            "an added image has no before"
        );

        git(repo, &["add", "logo.png"]);
        let (old, new) = sides(repo, &ChangeSource::Staged, "logo.png", None).unwrap();
        assert!(old.is_some() && new.is_some());

        git(repo, &["commit", "-qm", "second"]);
        let head = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let (old, new) = sides(
            repo,
            &ChangeSource::Commit {
                commit: head.trim().into(),
            },
            "logo.png",
            None,
        )
        .unwrap();
        assert!(old.is_some() && new.is_some());
    }

    #[test]
    fn between_two_snapshots_or_a_snapshot_and_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("logo.png"), png(b"one")).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "one"]);
        let rev = |repo: &Path| {
            String::from_utf8(
                Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(repo)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_string()
        };
        let first = rev(repo);
        std::fs::write(repo.join("logo.png"), png(b"two")).unwrap();
        git(repo, &["commit", "-qam", "two"]);
        let second = rev(repo);
        std::fs::write(repo.join("logo.png"), png(b"three")).unwrap();

        let (a, b) = sides_between(repo, &first, Some(&second), "logo.png", None).unwrap();
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.data_base64, b.data_base64);
        let (_, now) = sides_between(repo, &first, None, "logo.png", None).unwrap();
        assert_ne!(
            now.unwrap().data_base64,
            b.data_base64,
            "None is the worktree"
        );
        assert!(sides_between(repo, "HEAD~1", None, "logo.png", None).is_err());
        assert!(sides_between(repo, &first, None, "../x.png", None).is_err());
    }

    #[test]
    fn what_is_not_an_image_or_not_a_safe_path_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("blob.bin"), b"\x00\x01not an image").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "first"]);
        std::fs::write(repo.join("blob.bin"), b"\x00\x02still not").unwrap();
        assert_eq!(
            sides(repo, &ChangeSource::Uncommitted, "blob.bin", None).unwrap(),
            (None, None)
        );
        assert!(sides(repo, &ChangeSource::Uncommitted, "../outside.png", None).is_err());
        assert!(sides(repo, &ChangeSource::Uncommitted, "--output=x", None).is_err());
        assert!(
            sides(
                repo,
                &ChangeSource::Commit {
                    commit: "HEAD~1..".into()
                },
                "blob.bin",
                None
            )
            .is_err()
        );
    }
}
