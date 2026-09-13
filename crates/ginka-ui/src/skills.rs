//! View-state decisions for the agents' own skills library.

use ginka_protocol::model::{Skill, SkillInstall};

/// The one mutation a skill row asks the daemon to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleRequest {
    /// Skill name, which identifies every grouped installation.
    pub name: String,
    /// State every copy should have after the mutation.
    pub enabled: bool,
}

/// Build the all-copies toggle represented by a grouped skill row.
///
/// A partly disabled skill reports `enabled = false`, so its next action
/// restores every copy instead of hiding the copies that remain visible.
pub fn toggle_request(skill: &Skill) -> ToggleRequest {
    ToggleRequest {
        name: skill.name.clone(),
        enabled: !skill.enabled,
    }
}

/// Keep each installation available for the detail beneath a grouped row.
pub fn install_rows(skill: &Skill) -> &[SkillInstall] {
    &skill.installs
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::model::{Skill, SkillInstall, SkillScope};
    use std::path::PathBuf;

    fn skill(enabled: bool) -> Skill {
        Skill {
            name: "review-diff".into(),
            description: Some("Review a patch".into()),
            enabled,
            installs: vec![
                SkillInstall {
                    root_label: "claude".into(),
                    scope: SkillScope::User,
                    directory: PathBuf::from("/home/me/.claude/skills/review-diff"),
                    enabled: true,
                },
                SkillInstall {
                    root_label: "app".into(),
                    scope: SkillScope::Project,
                    directory: PathBuf::from("/repo/.agents/skills/review-diff"),
                    enabled,
                },
            ],
        }
    }

    #[test]
    fn a_disabled_or_partly_disabled_skill_is_enabled_as_one_operation() {
        let request = toggle_request(&skill(false));
        assert_eq!(request.name, "review-diff");
        assert!(request.enabled);
    }

    #[test]
    fn an_enabled_skill_is_disabled_as_one_operation() {
        assert!(!toggle_request(&skill(true)).enabled);
    }

    #[test]
    fn installs_keep_their_scope_and_daemon_host_path_for_the_detail() {
        let skill = skill(false);
        let rows = install_rows(&skill);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].root_label, "claude");
        assert_eq!(rows[0].scope, SkillScope::User);
        assert!(rows[0].directory.ends_with("review-diff"));
        assert!(!rows[1].enabled);
    }
}
