//! Testable policy for moving a conversation between agent CLIs.

use ginka_protocol::model::AgentStatus;

/// The first request given to a forked provider after the transcript digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForkAction {
    /// Open the fork without starting a turn.
    Continue,
    /// Ask the other provider to review the work without editing it.
    SecondOpinion,
    /// Ask the other provider to prepare a plan without editing files.
    Plan,
    /// Ask the other provider to implement the plan in the copied transcript.
    Build,
}

impl ForkAction {
    /// Localized first message; `None` leaves the new conversation idle.
    pub fn prompt(self, locale: &str) -> Option<String> {
        let key = match self {
            Self::Continue => return None,
            Self::SecondOpinion => "transcript.second_opinion.prompt",
            Self::Plan => "transcript.plan.prompt",
            Self::Build => "transcript.build.prompt",
        };
        Some(rust_i18n::t!(key, locale = locale).to_string())
    }
}

/// Installed and signed-in agents that can receive a conversation from the
/// current provider.
pub fn fork_targets<'a>(agents: &'a [AgentStatus], current: &str) -> Vec<&'a AgentStatus> {
    agents
        .iter()
        .filter(|agent| agent.id != current && agent.is_ready())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agent(id: &str, installed: bool, authenticated: Option<bool>) -> AgentStatus {
        AgentStatus {
            id: id.into(),
            display_name: id.to_uppercase(),
            program: id.into(),
            installed,
            version: None,
            authenticated,
            detail: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn targets_are_ready_agents_other_than_the_one_that_owns_the_conversation() {
        let agents = [
            agent("claude", true, Some(true)),
            agent("codex", true, None),
            agent("gemini", true, Some(false)),
            agent("missing", false, None),
        ];

        let targets = fork_targets(&agents, "claude");

        assert_eq!(
            targets
                .iter()
                .map(|agent| agent.id.as_str())
                .collect::<Vec<_>>(),
            vec!["codex"]
        );
    }

    #[test]
    fn a_conversation_with_no_alternative_cli_has_no_fork_target() {
        let agents = [agent("claude", true, Some(true))];
        assert!(fork_targets(&agents, "claude").is_empty());
    }

    #[test]
    fn plan_and_build_are_distinct_first_prompts_after_a_fork() {
        assert_eq!(ForkAction::Continue.prompt("en"), None);
        let review = ForkAction::SecondOpinion.prompt("en").unwrap();
        let plan = ForkAction::Plan.prompt("en").unwrap();
        let build = ForkAction::Build.prompt("en").unwrap();
        assert!(review.contains("Do not change any files"));
        assert!(plan.contains("Do not change any files"));
        assert!(plan.contains("implementation plan"));
        assert!(build.contains("Implement"));
        assert!(build.contains("tests"));
        assert_ne!(plan, build);
        assert_ne!(review, plan);
        let japanese_plan = ForkAction::Plan.prompt("ja").unwrap();
        let japanese_build = ForkAction::Build.prompt("ja").unwrap();
        assert!(japanese_plan.contains("ファイルは変更しない"));
        assert!(japanese_build.contains("テスト"));
        assert_ne!(japanese_plan, plan);
    }
}
