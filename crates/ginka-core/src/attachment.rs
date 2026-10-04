//! Files the user attaches to a message.
//!
//! Uploads are daemon-owned: the bytes live under the daemon's state
//! directory and the message keeps a `ginka-attachment:` reference, so a
//! transcript stays small and a client that is not on the daemon's host never
//! needs the original path. See `docs/roadmap.md` §3.3 N6.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Prefix of every attachment reference; the rest is the stored file name.
pub const ATTACHMENT_SCHEME: &str = "ginka-attachment:";

/// One upload is capped so a stray drag of a video file fails fast instead of
/// filling the disk and the wire buffer behind it.
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;

/// What the transcript keeps about an upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAttachment {
    /// `ginka-attachment:<id>` — what a message refers to it by.
    pub reference: String,
    /// Where it lives **on the daemon's host** (roadmap §4.1).
    pub path: PathBuf,
    /// The name the user knows it by. Display only: never a path component.
    pub name: String,
    /// Size of the upload in bytes; never zero and never over
    /// [`MAX_ATTACHMENT_BYTES`].
    pub bytes: usize,
}

/// The daemon's attachment directory, one flat file per upload named by a
/// fresh UUID plus a sanitized extension.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    /// A store rooted at `root`; the directory is created on the first `put`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory uploads are written into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store an upload under a fresh id.
    ///
    /// The id is generated rather than derived from the name: two files called
    /// `notes.md` are two attachments, and the second must not overwrite the
    /// first. The name the user typed never becomes a path component either,
    /// so `../../etc/passwd` is a display string and nothing more.
    pub fn put(&self, name: &str, bytes: &[u8]) -> Result<StoredAttachment> {
        if bytes.is_empty() {
            bail!("attachment {name} is empty");
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            bail!(
                "attachment {name} is too large: {} bytes against a limit of {MAX_ATTACHMENT_BYTES}",
                bytes.len()
            );
        }

        let id = Uuid::new_v4().simple().to_string();
        let stored_name = match extension_of(name) {
            Some(extension) => format!("{id}.{extension}"),
            None => id.clone(),
        };
        let path = self.root.join(&stored_name);

        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        let temp = path.with_extension("part");
        std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
        std::fs::rename(&temp, &path).with_context(|| format!("publishing {}", path.display()))?;

        Ok(StoredAttachment {
            reference: format!("{ATTACHMENT_SCHEME}{stored_name}"),
            path,
            name: name.to_string(),
            bytes: bytes.len(),
        })
    }

    /// The bytes behind a `ginka-attachment:` reference. `Ok(None)` for a
    /// malformed reference or a file that is gone; other I/O failures are errors.
    pub fn read(&self, reference: &str) -> Result<Option<Vec<u8>>> {
        let Some(path) = self.path_of(reference) else {
            return Ok(None);
        };
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Where a reference's file would be, without checking it exists. `None`
    /// unless it carries the scheme and a bare file name, so a reference can
    /// never escape the store.
    pub fn path_of(&self, reference: &str) -> Option<PathBuf> {
        let name = reference.strip_prefix(ATTACHMENT_SCHEME)?;
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
            return None;
        }
        Some(self.root.join(name))
    }
}

/// The extension is kept so a stored file is still openable by type; anything
/// that is not a short alphanumeric suffix is dropped rather than trusted.
fn extension_of(name: &str) -> Option<String> {
    let extension = Path::new(name).extension()?.to_str()?;
    let usable = extension.len() <= 8 && extension.chars().all(|c| c.is_ascii_alphanumeric());
    usable.then(|| extension.to_ascii_lowercase())
}

/// Replace every attachment reference in `text` with the file's path on the
/// daemon's host.
///
/// This is what an agent is given: it reads files, not URIs. A reference that
/// resolves to nothing is left exactly as it was — replacing it with an empty
/// string would hand the agent a sentence with a hole in it and no way to say
/// what went missing.
pub fn expand_references(text: &str, store: &AttachmentStore) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(ATTACHMENT_SCHEME) {
        out.push_str(&rest[..start]);
        let after = &rest[start + ATTACHMENT_SCHEME.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
            .unwrap_or(after.len());
        let (id, tail) = after.split_at(end);
        let reference = format!("{ATTACHMENT_SCHEME}{id}");

        match store.path_of(&reference).filter(|path| path.exists()) {
            Some(path) => out.push_str(&path.to_string_lossy()),
            // Not ours, gone, or trying to climb out of the store.
            None => out.push_str(&reference),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}
