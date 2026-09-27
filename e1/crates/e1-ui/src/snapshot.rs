//! What the window knew when it was last open, kept for the next launch.
//!
//! The HTTP cache makes a refresh cheap; this is what makes the first
//! frame full. Everything the store had is written as one JSON file and
//! read back before the first request goes out, so the window opens on
//! yesterday's inbox and replaces it a moment later rather than opening on
//! *Loading…*. It is a cache, not a record: a snapshot that fails to parse
//! is ignored, and signing out deletes it.

use crate::nav::Focus;
use e1_github::{Comment, Item, Notification, Project, ProjectBoard, Pull, Repo, RepoId, Viewer};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Everything the detail panel draws for one item, fetched together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemDetail {
    /// The item.
    pub item: Item,
    /// What only a pull has, when it is one.
    pub pull: Option<Pull>,
    /// Its comments, oldest first.
    pub comments: Vec<Comment>,
}

/// The store's memory, serialised.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// The file's own version. A snapshot from a build whose shapes differ
    /// is thrown away rather than half-read.
    pub version: u32,
    /// Who it belongs to.
    pub viewer: Option<Viewer>,
    /// The repositories.
    pub repos: Vec<Repo>,
    /// The inbox.
    pub inbox: Vec<Notification>,
    /// Every list that had landed.
    pub lists: Vec<(Focus, Vec<Item>)>,
    /// The items that had been read, most recent last.
    pub details: Vec<((RepoId, u64), ItemDetail)>,
    /// Every Project visible to the viewer.
    pub projects: Vec<Project>,
    /// Recently opened Projects, least recent first.
    pub project_boards: Vec<ProjectBoard>,
}

/// The shape this build writes.
pub const VERSION: u32 = 3;

/// How many read items are kept. The recent ones are what a reader comes
/// back to; the rest are one request away.
pub const DETAILS_KEPT: usize = 40;

/// How many complete Project boards are kept between launches.
pub const PROJECTS_KEPT: usize = 5;

impl Snapshot {
    /// An empty snapshot of this build's version.
    pub fn new() -> Self {
        Self {
            version: VERSION,
            ..Self::default()
        }
    }

    /// Keep only the last [`DETAILS_KEPT`] items.
    pub fn trim(&mut self) {
        let excess = self.details.len().saturating_sub(DETAILS_KEPT);
        self.details.drain(..excess);
        let excess = self.project_boards.len().saturating_sub(PROJECTS_KEPT);
        self.project_boards.drain(..excess);
    }
}

/// Read a snapshot. `None` when there is none, or when it is not one this
/// build can read.
pub fn load(path: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    let snapshot: Snapshot = serde_json::from_str(&text).ok()?;
    (snapshot.version == VERSION).then_some(snapshot)
}

/// Write a snapshot atomically.
pub fn save(path: &Path, snapshot: &Snapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec(snapshot)?)?;
    std::fs::rename(temp, path)
}

/// Delete a snapshot. Not an error when there was none.
pub fn forget(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::Section;
    use e1_github::GitHub as _;

    #[test]
    fn a_snapshot_round_trips_and_an_unknown_version_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache").join("store.json");
        assert_eq!(load(&path), None);

        let mut snapshot = Snapshot::new();
        snapshot.viewer = Some(Viewer {
            login: "bokuweb".into(),
            name: None,
            avatar_url: String::new(),
        });
        snapshot
            .lists
            .push((Focus::Section(Section::MyPulls), Vec::new()));
        save(&path, &snapshot).unwrap();
        assert_eq!(load(&path), Some(snapshot.clone()));

        snapshot.version = VERSION + 1;
        save(&path, &snapshot).unwrap();
        assert_eq!(load(&path), None, "another build's shapes are not read");

        forget(&path).unwrap();
        forget(&path).unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn garbage_is_ignored_rather_than_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn only_the_recent_items_are_kept() {
        let mut snapshot = Snapshot::new();
        let detail = |n: u64| {
            (
                (RepoId::new("o", "r"), n),
                ItemDetail {
                    item: e1_github::Scripted::sample()
                        .search("is:pr")
                        .ok()
                        .and_then(|items| items.into_iter().next())
                        .unwrap(),
                    pull: None,
                    comments: Vec::new(),
                },
            )
        };
        for n in 0..(DETAILS_KEPT as u64 + 5) {
            snapshot.details.push(detail(n));
        }
        snapshot.trim();
        assert_eq!(snapshot.details.len(), DETAILS_KEPT);
        assert_eq!(snapshot.details[0].0.1, 5, "the oldest go first");
    }

    #[test]
    fn only_the_recent_projects_are_kept() {
        let source = e1_github::Scripted::sample();
        let project = source.projects("bokuweb").unwrap().remove(0);
        let board = source.project(&project).unwrap();
        let mut snapshot = Snapshot::new();
        for index in 0..(PROJECTS_KEPT + 2) {
            let mut board = board.clone();
            board.project.id = format!("project-{index}");
            snapshot.project_boards.push(board);
        }

        snapshot.trim();

        assert_eq!(snapshot.project_boards.len(), PROJECTS_KEPT);
        assert_eq!(snapshot.project_boards[0].project.id, "project-2");
    }
}
