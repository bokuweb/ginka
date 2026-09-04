//! Content-addressed storage for the binary payloads agents emit.
//!
//! Providers hand back screenshots and generated images as `data:` URLs.
//! Keeping those in the transcript is what turns one screenshot-heavy session
//! into megabytes of state: base64 costs a third more than the bytes it
//! carries, and every render decodes the string again. Above a small
//! threshold the bytes go to a file and the transcript keeps a
//! `ginka-blob:` reference instead. See `docs/roadmap.md` §3.3 N6.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The scheme a stored payload is referenced by.
pub const BLOB_SCHEME: &str = "ginka-blob:";

/// Payloads at or below this stay inline: a reference plus a file is not worth
/// it for an icon, and a transcript full of tiny files is its own problem.
pub const INLINE_THRESHOLD_BYTES: usize = 8 * 1024;

/// Files under one directory, named by the digest of their contents.
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Replace a `data:` URL with a reference, storing the bytes.
    ///
    /// Anything that is not a data URL is returned untouched: callers run
    /// every image source through this, and an `https:` URL is already a
    /// reference to something we are not holding.
    pub fn externalize(&self, source: &str) -> Result<String> {
        let Some((mime, bytes)) = decode_data_url(source)? else {
            return Ok(source.to_string());
        };
        if bytes.len() <= INLINE_THRESHOLD_BYTES {
            return Ok(source.to_string());
        }
        self.put(&bytes, &mime)
    }

    /// Store bytes and return their reference. Identical bytes of the same
    /// type resolve to the same file, so a screenshot repeated across turns
    /// is stored once.
    pub fn put(&self, bytes: &[u8], mime: &str) -> Result<String> {
        let name = format!("{}.{}", digest_of(bytes), extension_for(mime));
        let path = self.root.join(&name);
        if !path.exists() {
            std::fs::create_dir_all(&self.root)
                .with_context(|| format!("creating {}", self.root.display()))?;
            // Written beside the target and renamed, so a crash mid-write
            // cannot leave a half file under a name that claims to be whole.
            let temp = path.with_extension("part");
            std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
            std::fs::rename(&temp, &path)
                .with_context(|| format!("publishing {}", path.display()))?;
        }
        Ok(format!("{BLOB_SCHEME}{name}"))
    }

    /// The bytes behind a reference, or `None` when it is not ours or no
    /// longer here. A missing blob is a degraded render, never an error that
    /// takes a transcript down with it.
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

    /// Where a reference lives, if it is one of ours and names a plain file.
    pub fn path_of(&self, reference: &str) -> Option<PathBuf> {
        let name = reference.strip_prefix(BLOB_SCHEME)?;
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
            return None;
        }
        Some(self.root.join(name))
    }

    /// How many blobs are stored. For tests and for the retention sweep.
    pub fn len(&self) -> Result<usize> {
        match std::fs::read_dir(&self.root) {
            Ok(entries) => Ok(entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext != "part"))
                .count()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error).context("listing the blob store"),
        }
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// `Some((mime, bytes))` for a base64 data URL, `None` for anything that is
/// not a data URL at all, and an error for one that is malformed — a provider
/// sending us broken base64 is a bug worth seeing, not one to store.
fn decode_data_url(source: &str) -> Result<Option<(String, Vec<u8>)>> {
    let Some(rest) = source.strip_prefix("data:") else {
        return Ok(None);
    };
    let Some((meta, payload)) = rest.split_once(',') else {
        bail!("data URL has no comma separating its metadata from its payload");
    };
    let mime = meta.split(';').next().unwrap_or_default();
    if !meta.split(';').any(|part| part == "base64") {
        bail!("only base64 data URLs are stored; this one is percent-encoded");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .context("data URL payload is not valid base64")?;
    Ok(Some((mime.to_string(), bytes)))
}

fn digest_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    // Half a SHA-256 is far past what a per-user store needs to avoid a
    // collision, and keeps the reference short enough to read in a log line.
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}
