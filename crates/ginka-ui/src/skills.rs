//! View-state decisions for the agents' own skills library.

use ginka_protocol::model::{Skill, SkillInstall, SkillScope};

/// Which installed scope remains visible in the skills library.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ScopeFilter {
    /// Skills installed anywhere in the current catalogue.
    #[default]
    All,
    /// Skills with at least one user-scoped installation.
    User,
    /// Skills with at least one project-scoped installation.
    Project,
}

/// Which grouped enablement state remains visible in the skills library.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    /// Skills in either enablement state.
    #[default]
    All,
    /// Skills whose every installed copy is enabled.
    Enabled,
    /// Skills with one or more disabled copies.
    Disabled,
}

/// The composable controls above the skills library.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillFilter {
    /// Case-insensitive text matched against metadata and daemon-host paths.
    pub query: String,
    /// Required install scope.
    pub scope: ScopeFilter,
    /// Required grouped enablement state.
    pub state: StateFilter,
}

/// Keep skills matching every active filter, without disturbing catalogue order.
pub fn filter_skills<'a>(skills: &'a [Skill], filter: &SkillFilter) -> Vec<&'a Skill> {
    let query = filter.query.trim().to_lowercase();
    skills
        .iter()
        .filter(|skill| match filter.scope {
            ScopeFilter::All => true,
            ScopeFilter::User => skill
                .installs
                .iter()
                .any(|install| install.scope == SkillScope::User),
            ScopeFilter::Project => skill
                .installs
                .iter()
                .any(|install| install.scope == SkillScope::Project),
        })
        .filter(|skill| match filter.state {
            StateFilter::All => true,
            StateFilter::Enabled => skill.enabled,
            StateFilter::Disabled => !skill.enabled,
        })
        .filter(|skill| {
            query.is_empty()
                || skill.name.to_lowercase().contains(&query)
                || skill
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_lowercase().contains(&query))
                || skill.installs.iter().any(|install| {
                    install.root_label.to_lowercase().contains(&query)
                        || install
                            .directory
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&query)
                })
        })
        .collect()
}

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

/// Render the daemon-host directory exactly as the path action copies it.
pub fn install_path_text(install: &SkillInstall) -> String {
    install.directory.to_string_lossy().into_owned()
}

/// Return a local directory URL only when the daemon's paths belong to this
/// machine and the install still exists. External daemon paths remain copyable.
pub fn install_directory_url(install: &SkillInstall, local_paths: bool) -> Option<String> {
    if !local_paths || !install.directory.is_dir() {
        return None;
    }
    url::Url::from_directory_path(&install.directory)
        .ok()
        .map(Into::into)
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
        assert_eq!(
            install_path_text(&rows[0]),
            "/home/me/.claude/skills/review-diff"
        );
        assert!(!rows[1].enabled);
    }

    #[test]
    fn skills_match_names_descriptions_roots_and_daemon_paths() {
        let skills = vec![skill(false)];
        for query in ["review", "patch", "claude", ".agents/skills"] {
            let filtered = filter_skills(
                &skills,
                &SkillFilter {
                    query: query.into(),
                    ..SkillFilter::default()
                },
            );
            assert_eq!(filtered.len(), 1, "query {query:?} should match");
        }
        assert!(
            filter_skills(
                &skills,
                &SkillFilter {
                    query: "deploy".into(),
                    ..SkillFilter::default()
                },
            )
            .is_empty()
        );
    }

    #[test]
    fn scope_and_state_filters_compose_with_text_search() {
        let mut project_and_user = skill(false);
        project_and_user.name = "shared-review".into();
        let mut user_only = skill(true);
        user_only.name = "user-review".into();
        user_only.installs.truncate(1);
        let skills = vec![project_and_user, user_only];

        let filtered = filter_skills(
            &skills,
            &SkillFilter {
                query: "review".into(),
                scope: ScopeFilter::Project,
                state: StateFilter::Disabled,
            },
        );
        assert_eq!(
            filtered
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            ["shared-review"]
        );

        assert!(
            filter_skills(
                &skills,
                &SkillFilter {
                    query: String::new(),
                    scope: ScopeFilter::Project,
                    state: StateFilter::Enabled,
                },
            )
            .is_empty()
        );
    }

    #[test]
    fn local_install_directory_opens_as_an_encoded_file_url() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("review notes 日本語");
        std::fs::create_dir(&directory).unwrap();
        let mut install = skill(true).installs.remove(0);
        install.directory = directory.clone();

        let url = install_directory_url(&install, true).unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.scheme(), "file");
        assert_eq!(parsed.to_file_path().unwrap(), directory);
    }

    #[test]
    fn external_or_missing_skill_directory_cannot_be_opened_locally() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("skill");
        std::fs::create_dir(&directory).unwrap();
        let mut install = skill(true).installs.remove(0);
        install.directory = directory.clone();

        assert_eq!(install_directory_url(&install, false), None);
        std::fs::remove_dir(&directory).unwrap();
        assert_eq!(install_directory_url(&install, true), None);
    }
}
