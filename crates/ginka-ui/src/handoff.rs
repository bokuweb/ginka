//! Testable policy for moving a conversation between agent CLIs.

use ginka_protocol::model::AgentStatus;

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
}
