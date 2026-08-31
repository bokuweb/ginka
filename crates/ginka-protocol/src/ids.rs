use serde::{Deserialize, Serialize};
use std::fmt;

/// A project's stable key. Projects are keyed by name rather than by path so a
/// repository can be moved on disk without orphaning its workspaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectName(pub String);

/// Identifies one workspace (one git worktree).
///
/// Derived from the worktree's immutable `name`, **never** from the live branch
/// — an agent switching branches inside a worktree must not re-key everything
/// that hangs off this id. See `docs/roadmap.md` §4.4.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub String);

impl WorkspaceId {
    /// Build the id from a project name and the worktree's immutable name.
    pub fn new(project: &ProjectName, worktree_name: &str) -> Self {
        Self(format!("{}/{}", project.0, slugify(worktree_name)))
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
}
