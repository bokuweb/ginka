//! Stable identifiers, and the slug rule every one of them is built with.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A project's stable key. Projects are keyed by name rather than by path so a
/// repository can be moved on disk without orphaning its workspaces.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "export", ts(type = "string"))]
pub struct ProjectName(pub String);

/// Identifies one workspace (one git worktree).
///
/// Derived from the worktree's immutable `name`, **never** from the live branch
/// — an agent switching branches inside a worktree must not re-key everything
/// that hangs off this id. See `docs/roadmap.md` §4.4.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "export", ts(type = "string"))]
pub struct WorkspaceId(pub String);

impl WorkspaceId {
    /// Build the id from a project name and the worktree's immutable name.
    pub fn new(project: &ProjectName, worktree_name: &str) -> Self {
        Self(format!("{}/{}", project.0, slugify(worktree_name)))
    }

    /// Split the id back into the project name and the worktree name.
    ///
    /// Returns `None` for a string that was not produced by [`WorkspaceId::new`],
    /// which is how a request naming a workspace that cannot exist is rejected
    /// before it reaches the database.
    pub fn parts(&self) -> Option<(ProjectName, &str)> {
        let (project, name) = self.0.split_once('/')?;
        if project.is_empty() || name.is_empty() {
            return None;
        }
        Some((ProjectName(project.to_string()), name))
    }
}

/// Identifies one agent session — a single conversation with one driver in one
/// workspace. Opaque, assigned by the daemon; clients never construct one.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "export", ts(type = "string"))]
pub struct SessionId(pub String);

/// Identifies one terminal: a shell running in a workspace.
///
/// Opaque, assigned by the daemon, which is what owns the pty.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[cfg_attr(feature = "export", ts(type = "string"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(pub String);

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies one account: one login of one provider (`docs/accounts.md` §3).
///
/// A slug, immutable once created, and global across providers. The default
/// account of a provider has the provider's own id — `claude`, `codex` — and
/// never appears in the settings file, because it is the vendor's own home.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "export", ts(type = "string"))]
pub struct AccountId(pub String);

impl AccountId {
    /// The implicit account of a provider: its own home, as it is today.
    pub fn default_for(provider: crate::provider::ProviderKind) -> Self {
        Self(provider.as_str().to_string())
    }

    /// Whether this is some provider's implicit account.
    pub fn is_default(&self) -> bool {
        crate::provider::ProviderKind::parse(&self.0).is_some()
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AccountId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Identifies one checkpoint: a workspace state snapshotted at a turn boundary.
#[cfg_attr(feature = "export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "export", ts(type = "string"))]
pub struct CheckpointId(pub String);
impl From<&str> for WorkspaceId {
    /// Adopt an id that was already built — read back from the database or
    /// received over the wire. Ids are *created* with [`WorkspaceId::new`];
    /// this is how one that already exists comes home.
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for WorkspaceId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Lowercase, collapse every run of non-alphanumeric characters to a single
/// `-`, and trim the result. Branch names carry `/`, `.` and unicode; ids must
/// survive being used in paths, URLs and shell arguments.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_collapses_separators() {
        assert_eq!(
            slugify("zeron/repository-story-creation"),
            "zeron-repository-story-creation"
        );
        assert_eq!(slugify("feature/JIRA-123_fix"), "feature-jira-123-fix");
        assert_eq!(slugify("  leading and trailing  "), "leading-and-trailing");
        assert_eq!(slugify("日本語"), "");
    }

    #[test]
    fn workspace_id_uses_worktree_name_not_branch() {
        let project = ProjectName("comet".into());
        let id = WorkspaceId::new(&project, "bright-harbor");
        assert_eq!(id.0, "comet/bright-harbor");
    }

    #[test]
    fn a_workspace_id_splits_back_into_its_parts() {
        let id = WorkspaceId::new(&ProjectName("comet".into()), "bright-harbor");
        let (project, name) = id.parts().expect("a well-formed id splits");
        assert_eq!(project.0, "comet");
        assert_eq!(name, "bright-harbor");
    }

    #[test]
    fn a_malformed_workspace_id_has_no_parts() {
        assert!(WorkspaceId("comet".into()).parts().is_none());
        assert!(WorkspaceId("/harbor".into()).parts().is_none());
        assert!(WorkspaceId("comet/".into()).parts().is_none());
    }
}
