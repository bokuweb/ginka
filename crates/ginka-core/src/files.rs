//! The files in a workspace, and finding one by typing part of its name.
//!
//! Listing goes through `git ls-files` rather than walking the directory: it
//! honours `.gitignore` for free, which is the difference between offering the
//! user their own source files and offering them `node_modules`. Untracked
//! files are included — a file the agent wrote a minute ago is exactly the one
//! being reached for.
//!
//! Matching is `nucleo`, the matcher the roadmap picked (§4.3): a path is
//! matched by typing any subsequence of it, and the ranking understands that a
//! hit in the file name beats one in a directory three levels up.

use crate::git;
use anyhow::{Context as _, Result};
use base64::Engine as _;
use ginka_protocol::model::{ContentMatch, FileContent, FileEntry, FileImage};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use sha2::{Digest as _, Sha256};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// How many paths a search answers with by default.
///
/// A menu is read, not scrolled: past this the user is better served by typing
/// another character than by a longer list.
pub const DEFAULT_LIMIT: usize = 30;

/// Every file in the worktree that git would show, tracked or not.
pub fn list(worktree: &Path) -> Result<Vec<String>> {
    // `--others --exclude-standard` adds untracked files while still obeying
    // the ignore rules; `--cached` keeps the tracked ones. Deduplicated,
    // because a file can be both when it is staged and modified.
    let listed = git::ls_files(worktree)?;
    let mut paths: Vec<String> = listed;
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

/// The lines in a workspace's files that contain `query`.
///
/// A literal search rather than a regular expression: what a reader types into
/// a find box is what they are looking for, and a stray `(` turning into a
/// syntax error is a worse answer than no match. An empty query matches
/// nothing rather than everything.
pub fn search_content(worktree: &Path, query: &str, limit: usize) -> Result<Vec<ContentMatch>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    Ok(git::grep(worktree, query, limit)?
        .into_iter()
        .map(|(path, line, text)| ContentMatch {
            path,
            line,
            // Long lines are a minified file's, and a list of them is a list
            // of nothing readable.
            text: text.trim_end().chars().take(200).collect(),
        })
        .collect())
}

/// How much of a file the viewer is given.
///
/// A minified bundle or a megabyte of generated JSON is not something anyone
/// reads in a side panel, and shipping it over the socket to be laid out as
/// text is how a window stops responding. What is worth reading is at the top.
pub const READ_LIMIT: usize = 512 * 1024;

/// Largest image carried inline by a file-read response.
///
/// Base64 expands this to roughly 5.4 MiB, leaving ample room below the wire
/// frame limit while keeping a mistaken large asset from stalling the window.
pub const IMAGE_PREVIEW_LIMIT: usize = 4 * 1024 * 1024;

/// Read a file from a workspace, for the panel that shows it.
///
/// The path is resolved inside the worktree and refused if it leads out of it:
/// a client is not the one that decides which of the user's files the daemon
/// reads, and `../` is the whole of that attack.
pub fn read(worktree: &Path, path: &str) -> Result<FileContent> {
    let inside = resolve_file(worktree, path)?;
    content(path, &inside)
}

/// Replace an existing text file if it is still the revision the client read.
///
/// Written through a sibling temporary file so a crash cannot leave a
/// half-written source file. The original permissions are retained.
pub fn write(
    worktree: &Path,
    path: &str,
    text: &str,
    expected_revision: &str,
) -> Result<FileContent> {
    anyhow::ensure!(
        text.len() <= READ_LIMIT,
        "{path} is too large to edit in Ginka"
    );
    let inside = resolve_file(worktree, path)?;
    let bytes = std::fs::read(&inside).with_context(|| format!("reading {path} before saving"))?;
    anyhow::ensure!(
        revision(&bytes) == expected_revision,
        "{path} changed since it was opened; reopen it before saving"
    );
    anyhow::ensure!(
        !bytes[..bytes.len().min(8000)].contains(&0) && std::str::from_utf8(&bytes).is_ok(),
        "{path} is binary and cannot be edited"
    );

    let permissions = std::fs::metadata(&inside)
        .with_context(|| format!("reading permissions for {path}"))?
        .permissions();
    let name = inside
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temp = inside.with_file_name(format!(".{name}.ginka-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&temp, text).with_context(|| format!("writing a replacement for {path}"))?;
        std::fs::set_permissions(&temp, permissions)
            .with_context(|| format!("preserving permissions for {path}"))?;
        std::fs::rename(&temp, &inside).with_context(|| format!("replacing {path}"))?;
        content(path, &inside)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Resolve an existing file without allowing a symlink to leave the worktree.
pub(crate) fn resolve_file(worktree: &Path, path: &str) -> Result<std::path::PathBuf> {
    let inside = worktree
        .join(path)
        .canonicalize()
        .with_context(|| format!("no file at {path}"))?;
    let root = worktree
        .canonicalize()
        .context("the worktree is not where it was")?;
    anyhow::ensure!(inside.starts_with(&root), "{path} is outside the workspace");
    anyhow::ensure!(inside.is_file(), "{path} is not a file");
    Ok(inside)
}

/// Read an already-resolved file into its wire shape.
fn content(path: &str, inside: &Path) -> Result<FileContent> {
    let mut source = std::fs::File::open(inside).with_context(|| format!("reading {path}"))?;
    let length = source
        .metadata()
        .with_context(|| format!("reading metadata for {path}"))?
        .len();
    let mut header = [0_u8; 12];
    let header_length = source
        .read(&mut header)
        .with_context(|| format!("reading {path}"))?;
    let image_type = image_media_type(&header[..header_length]);
    source
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("reading {path}"))?;

    if image_type.is_some() {
        let (revision, bytes) =
            image_content(&mut source).with_context(|| format!("reading {path}"))?;
        let Some(bytes) = bytes else {
            return Ok(FileContent {
                path: path.to_string(),
                text: String::new(),
                revision,
                binary: true,
                truncated: true,
                image: None,
            });
        };
        return Ok(FileContent {
            path: path.to_string(),
            text: String::new(),
            revision,
            binary: true,
            truncated: false,
            image: preview_image(&bytes),
        });
    }

    let mut bytes = Vec::with_capacity(length.min(IMAGE_PREVIEW_LIMIT as u64) as usize);
    source
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {path}"))?;
    let revision = revision(&bytes);
    // A NUL in the first few kilobytes is git's own test for a binary file,
    // and it is the right one: a viewer that printed a PNG as text would be
    // showing nothing and taking a long time about it.
    let looked_at = bytes.len().min(8000);
    if bytes[..looked_at].contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(FileContent {
            path: path.to_string(),
            text: String::new(),
            revision,
            binary: true,
            truncated: false,
            image: None,
        });
    }
    let truncated = bytes.len() > READ_LIMIT;
    let kept = if truncated {
        &bytes[..boundary(&bytes, READ_LIMIT)]
    } else {
        &bytes[..]
    };
    Ok(FileContent {
        path: path.to_string(),
        text: String::from_utf8_lossy(kept).to_string(),
        revision,
        binary: false,
        truncated,
        image: None,
    })
}

/// Build a controlled inline preview for recognized, bounded image bytes.
///
/// Detection uses the file signature rather than a caller-supplied extension
/// or MIME type. The same rule serves workspace files and composer uploads so
/// neither view attempts to decode arbitrary binary content as an image.
pub fn preview_image(bytes: &[u8]) -> Option<FileImage> {
    if bytes.len() > IMAGE_PREVIEW_LIMIT {
        return None;
    }
    let media_type = image_media_type(bytes)?;
    Some(FileImage {
        media_type: media_type.to_string(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// Recognize image formats the GPUI image view can decode without consulting
/// a repository-controlled extension or MIME declaration.
fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Stable opaque revision for optimistic saves.
fn revision(bytes: &[u8]) -> String {
    shortened_revision(Sha256::digest(bytes))
}

/// Read and hash an image while retaining at most the preview limit.
///
/// The stream continues after the cap so the revision still names the whole
/// file; its retained bytes are dropped as soon as the preview no longer fits.
fn image_content(reader: &mut impl std::io::Read) -> std::io::Result<(String, Option<Vec<u8>>)> {
    let mut digest = Sha256::new();
    let mut bytes = Some(Vec::with_capacity(IMAGE_PREVIEW_LIMIT));
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        if let Some(retained) = bytes.as_mut() {
            if retained.len() + read <= IMAGE_PREVIEW_LIMIT {
                retained.extend_from_slice(&buffer[..read]);
            } else {
                bytes = None;
            }
        }
    }
    Ok((shortened_revision(digest.finalize()), bytes))
}

fn shortened_revision(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The nearest character boundary at or before `end`.
///
/// The cut is arbitrary, and half a character at the end of it is a
/// replacement glyph in the middle of the last line someone reads.
fn boundary(bytes: &[u8], end: usize) -> usize {
    let mut end = end.min(bytes.len());
    // A continuation byte is `10xxxxxx`; the start of a character is not.
    while end > 0 && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    end
}

/// The paths that match `query`, best first.
///
/// An empty query is not a match of everything in an arbitrary order: it is
/// the start of the list, which is what a picker shows before anything is
/// typed.
pub fn search(paths: &[String], query: &str, limit: usize) -> Vec<FileEntry> {
    let query = query.trim();
    if query.is_empty() {
        return paths
            .iter()
            .take(limit)
            .map(|path| entry(path, 0))
            .collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, &String)> = pattern
        .match_list(paths.iter(), &mut matcher)
        .into_iter()
        .map(|(path, score)| (score, path))
        .collect();
    // Stable on the score, then on the path, so the same query always answers
    // in the same order.
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(score, path)| entry(path, score))
        .collect()
}

/// One path, with the part a list shows separately.
fn entry(path: &str, score: u32) -> FileEntry {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    FileEntry {
        path: path.to_string(),
        name,
        score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        [
            "src/main.rs",
            "src/shell.rs",
            "crates/ginka-core/src/service.rs",
            "crates/ginka-core/src/session.rs",
            "docs/ui.md",
            "README.md",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    #[test]
    fn typing_part_of_a_name_finds_it() {
        let found = search(&paths(), "service", 10);
        assert_eq!(found[0].path, "crates/ginka-core/src/service.rs");
        assert_eq!(found[0].name, "service.rs", "a list shows the name apart");
    }

    #[test]
    fn a_subsequence_is_enough() {
        // What makes a picker usable: `srvc` for `service.rs`.
        let found = search(&paths(), "srvc", 10);
        assert!(
            found.iter().any(|entry| entry.path.ends_with("service.rs")),
            "{found:?}"
        );
    }

    #[test]
    fn a_hit_in_the_name_beats_one_in_a_directory() {
        // `ui` appears in `docs/ui.md` as the name and in nothing else here;
        // the ranking must not bury it under a path that merely contains the
        // letters.
        let found = search(&paths(), "ui", 10);
        assert_eq!(found[0].path, "docs/ui.md", "{found:?}");
    }

    #[test]
    fn nothing_typed_is_the_start_of_the_list_rather_than_a_shuffle() {
        let found = search(&paths(), "", 3);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].path, "src/main.rs");
    }

    #[test]
    fn a_query_that_matches_nothing_answers_with_nothing() {
        assert!(search(&paths(), "zzzz", 10).is_empty());
    }

    #[test]
    fn the_list_is_bounded_because_a_menu_is_read_not_scrolled() {
        let many: Vec<String> = (0..500).map(|index| format!("file{index}.rs")).collect();
        assert_eq!(search(&many, "file", 10).len(), 10);
    }

    #[test]
    fn the_same_query_always_answers_in_the_same_order() {
        let first = search(&paths(), "rs", 10);
        let again = search(&paths(), "rs", 10);
        assert_eq!(first, again);
    }

    #[test]
    fn a_file_is_read_as_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let read = read(dir.path(), "src/main.rs").unwrap();
        assert_eq!(read.text, "fn main() {}\n");
        assert!(!read.binary);
        assert!(!read.truncated);
        assert!(!read.revision.is_empty());
    }

    #[test]
    fn invalid_utf8_is_read_only_instead_of_being_lossily_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("encoded.txt"), [0xff, b'a']).unwrap();

        let read = read(dir.path(), "encoded.txt").unwrap();
        assert!(read.binary);
        assert!(read.text.is_empty());
        let error = write(dir.path(), "encoded.txt", "replacement", &read.revision).unwrap_err();
        assert!(error.to_string().contains("binary"), "{error}");
    }

    #[test]
    fn saving_needs_the_revision_that_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "first\n").unwrap();
        let opened = read(dir.path(), "notes.md").unwrap();

        std::fs::write(&path, "changed elsewhere\n").unwrap();
        let error = write(dir.path(), "notes.md", "from editor\n", &opened.revision)
            .expect_err("an old editor must not overwrite a newer file");
        assert!(error.to_string().contains("changed since it was opened"));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "changed elsewhere\n"
        );
    }

    #[test]
    fn saving_returns_the_new_revision_and_preserves_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        std::fs::write(&path, "old\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        let opened = read(dir.path(), "script.sh").unwrap();

        let saved = write(dir.path(), "script.sh", "new\n", &opened.revision).unwrap();
        assert_eq!(saved.text, "new\n");
        assert_ne!(saved.revision, opened.revision);
        assert!(std::fs::metadata(path).unwrap().permissions().readonly());
    }

    #[test]
    fn saving_through_a_symlink_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, worktree.join("link.txt")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, worktree.join("link.txt")).unwrap();

        let error = write(&worktree, "link.txt", "replace\n", "stale").unwrap_err();
        assert!(
            error.to_string().contains("outside the workspace"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "secret\n");
    }

    #[test]
    fn a_path_that_leads_out_of_the_workspace_is_refused() {
        // The client does not get to choose which of the user's files the
        // daemon reads, and `../` is the whole of that attack.
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(dir.path().join("secrets"), "not yours\n").unwrap();

        let error = read(&worktree, "../secrets").unwrap_err();
        assert!(
            error.to_string().contains("outside the workspace"),
            "{error}"
        );
    }

    #[test]
    fn a_binary_file_says_so_rather_than_printing_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("logo.png"), [0x89, b'P', 0x00, b'N']).unwrap();

        let read = read(dir.path(), "logo.png").unwrap();
        assert!(read.binary);
        assert!(read.text.is_empty());
    }

    #[test]
    fn a_supported_image_carries_a_bounded_inline_preview() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01];
        std::fs::write(dir.path().join("logo.asset"), bytes).unwrap();

        let read = read(dir.path(), "logo.asset").unwrap();
        let image = read
            .image
            .expect("the PNG signature, not its extension, decides");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                image.data_base64,
            )
            .unwrap(),
            bytes
        );
        assert!(read.binary);
        assert!(!read.truncated);
    }

    #[test]
    fn raw_image_bytes_use_the_same_safe_preview_rule_as_workspace_files() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        let preview = preview_image(&png).expect("a PNG signature is previewable");
        assert_eq!(preview.media_type, "image/png");
        assert_eq!(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                preview.data_base64,
            )
            .unwrap(),
            png
        );
        assert!(preview_image(&[0x00, 0x01, 0x02]).is_none());
        assert!(preview_image(&vec![0x89; IMAGE_PREVIEW_LIMIT + 1]).is_none());
    }

    #[test]
    fn an_image_extension_does_not_make_arbitrary_binary_previewable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("not-really.png"), [0x00, 0x01, 0x02]).unwrap();

        let read = read(dir.path(), "not-really.png").unwrap();
        assert!(read.binary);
        assert!(read.image.is_none());
    }

    #[test]
    fn an_oversized_image_is_not_put_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = vec![0_u8; IMAGE_PREVIEW_LIMIT + 1];
        bytes[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        std::fs::write(dir.path().join("huge.png"), bytes).unwrap();

        let read = read(dir.path(), "huge.png").unwrap();
        assert!(read.binary);
        assert!(read.image.is_none());
        assert!(
            read.truncated,
            "the UI must explain why no preview was sent"
        );
    }

    #[test]
    fn a_file_too_big_to_read_in_a_panel_is_cut_where_a_character_ends() {
        let dir = tempfile::tempdir().unwrap();
        // Multi-byte on purpose: the cut lands mid-character unless it is
        // moved back, and a replacement glyph in the last line is a bug the
        // reader would blame on their file.
        let text = "あ".repeat(READ_LIMIT);
        std::fs::write(dir.path().join("huge.txt"), &text).unwrap();

        let read = read(dir.path(), "huge.txt").unwrap();
        assert!(read.truncated, "it did not fit and has to say so");
        assert!(read.text.len() <= READ_LIMIT);
        assert!(
            !read.text.contains('\u{FFFD}'),
            "the cut landed inside a character"
        );
    }

    #[test]
    fn a_file_that_is_not_there_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let error = read(dir.path(), "absent.rs").unwrap_err();
        assert!(error.to_string().contains("absent.rs"), "{error}");
    }
}
