//! Scope selection for the Files surface search.
//!
//! The view draws the chips, while this module decides which daemon identity
//! a query is addressed to. Keeping that decision here makes it testable
//! without compiling the GPUI view chains in the binary crate.

use ginka_protocol::{ProjectName, WorkspaceId};

/// How broadly the Files surface searches source contents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileSearchScope {
    /// Search only the workspace currently shown in the centre column.
    #[default]
    Workspace,
    /// Search every active workspace registered under the selected project.
    Project,
}

/// The daemon-side identity a scoped query is sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSearchTarget {
    /// One immutable workspace.
    Workspace(WorkspaceId),
    /// One registered project and all of its active worktrees.
    Project(ProjectName),
}

impl FileSearchScope {
    /// Resolve the current selection into one shared-protocol search target.
    pub fn target(self, workspace: WorkspaceId, project: ProjectName) -> FileSearchTarget {
        match self {
            Self::Workspace => FileSearchTarget::Workspace(workspace),
            Self::Project => FileSearchTarget::Project(project),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_scope_uses_the_selected_project_not_a_parsed_workspace_name() {
        let target = FileSearchScope::Project.target(
            WorkspaceId("old-name/main".into()),
            ProjectName("renamed-project".into()),
        );
        assert_eq!(
            target,
            FileSearchTarget::Project(ProjectName("renamed-project".into()))
        );
    }
}
