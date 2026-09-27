//! Answers GitHub already gave, kept on disk with their `ETag`.
//!
//! GitHub honours `If-None-Match`: a request whose answer has not changed
//! comes back as a `304` with no body, and — the part that matters — does
//! not count against the rate limit. So every answer is kept beside its
//! tag, and a refresh costs a round trip but neither the bytes nor the
//! budget. The cache is keyed by URL and knows nothing about what the body
//! means; the client decodes it the same way whether it came from GitHub
//! or from here.

use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

/// One kept answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cached {
    /// What GitHub tagged the answer with; sent back as `If-None-Match`.
    pub etag: String,
    /// The body, as GitHub sent it.
    pub body: String,
    /// The `next` page, when the answer had one. Kept because a `304`
    /// carries no `Link` header and a listing has to keep walking.
    pub next: Option<String>,
}

/// A directory of kept answers.
#[derive(Debug, Clone)]
pub struct HttpCache {
    dir: PathBuf,
}

impl HttpCache {
    /// A cache in this directory, created on first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Where an answer for this URL lives. A hash rather than the URL
    /// itself: URLs carry characters filesystems dislike, and a query string
    /// long enough to matter would be a file name long enough to fail.
    fn path(&self, url: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        self.dir.join(format!("{:016x}.json", hasher.finish()))
    }

    /// The kept answer, if there is one that still parses.
    pub fn load(&self, url: &str) -> Option<Cached> {
        let text = std::fs::read_to_string(self.path(url)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Keep an answer, atomically: a crash mid-write must not leave a file
    /// that parses as half an answer.
    pub fn store(&self, url: &str, cached: &Cached) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(url);
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec(cached)?)?;
        std::fs::rename(temp, path)
    }

    /// Forget everything: what signing out does, so the next account does
    /// not read the last one's inbox out of a `304`.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_comes_back_with_its_tag_and_its_next_page() {
        let dir = std::env::temp_dir().join(format!("e1-cache-{}", std::process::id()));
        let cache = HttpCache::new(&dir);
        let url = "https://api.github.com/user/repos?per_page=100&sort=pushed";
        assert_eq!(cache.load(url), None);
        let cached = Cached {
            etag: "W/\"abc\"".into(),
            body: "[]".into(),
            next: Some("https://api.github.com/user/repos?page=2".into()),
        };
        cache.store(url, &cached).unwrap();
        assert_eq!(cache.load(url), Some(cached));
        // Another URL is another file.
        assert_eq!(cache.load("https://api.github.com/user"), None);
        cache.clear().unwrap();
        assert_eq!(cache.load(url), None);
        // Clearing what is already gone is fine.
        cache.clear().unwrap();
    }

    #[test]
    fn a_url_becomes_a_short_file_name() {
        let cache = HttpCache::new("/tmp/x");
        let name = cache
            .path("https://api.github.com/search/issues?q=is:pr+review-requested:%40me")
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(name.ends_with(".json"));
        assert_eq!(name.len(), 16 + 5);
    }
}
