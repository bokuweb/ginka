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

/// Somewhere a conversation can be forked to: another agent on its default
/// model, or — when there is no other agent — another model of this one.
#[derive(Clone, Copy, Debug)]
pub struct ForkTarget<'a> {
    /// The installed, signed-in agent that receives the fork.
    pub agent: &'a AgentStatus,
    /// `None` is the agent's own default.
    pub model: Option<&'a ginka_protocol::provider::ProviderModel>,
}

impl ForkTarget<'_> {
    /// What the target's button says: the agent, and the model when it is
    /// a model of the same agent that is being chosen.
    pub fn label(&self) -> String {
        match self.model {
            Some(model) => format!("{} · {}", self.agent.display_name, model.label),
            None => self.agent.display_name.clone(),
        }
    }
}

/// Where a conversation on `current` (running `current_model`, or the
/// provider's default when `None`) can be forked to.
///
/// Every other ready agent comes first and alone when there is one: another
/// vendor is the stronger second opinion, and three actions per target per
/// model would make a menu nobody reads. With no other agent ready, the
/// current one's other models are offered instead (MonoCode 0.3.0), never the
/// model that wrote the answer.
pub fn model_fork_targets<'a>(
    agents: &'a [AgentStatus],
    current: &str,
    current_model: Option<&str>,
) -> Vec<ForkTarget<'a>> {
    let others: Vec<ForkTarget<'a>> = fork_targets(agents, current)
        .into_iter()
        .map(|agent| ForkTarget { agent, model: None })
        .collect();
    if !others.is_empty() {
        return others;
    }
    let Some(agent) = agents
        .iter()
        .find(|agent| agent.id == current && agent.is_ready())
    else {
        return Vec::new();
    };
    let answering = current_model.map(str::to_string).or_else(|| {
        agent
            .models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| agent.models.first())
            .map(|model| model.id.clone())
    });
    agent
        .models
        .iter()
        .filter(|model| Some(&model.id) != answering.as_ref())
        .map(|model| ForkTarget {
            agent,
            model: Some(model),
        })
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

    fn with_models(mut status: AgentStatus, models: &[(&str, bool)]) -> AgentStatus {
        status.models = models
            .iter()
            .map(|(id, is_default)| {
                let mut model =
                    ginka_protocol::provider::ProviderModel::new(*id, id.to_uppercase());
                model.is_default = *is_default;
                model
            })
            .collect();
        status
    }

    fn labels(targets: &[ForkTarget<'_>]) -> Vec<(String, Option<String>)> {
        targets
            .iter()
            .map(|target| {
                (
                    target.agent.id.clone(),
                    target.model.map(|model| model.id.clone()),
                )
            })
            .collect()
    }

    #[test]
    fn a_lone_provider_offers_its_other_models_instead() {
        // MonoCode 0.3.0: with one provider signed in, a second opinion is
        // still worth having from another of its models.
        let agents = [with_models(
            agent("claude", true, Some(true)),
            &[("opus", true), ("sonnet", false), ("haiku", false)],
        )];
        assert_eq!(
            labels(&model_fork_targets(&agents, "claude", Some("opus"))),
            vec![
                ("claude".into(), Some("sonnet".into())),
                ("claude".into(), Some("haiku".into())),
            ],
            "the model that wrote the answer is not asked to review it"
        );
        assert_eq!(
            labels(&model_fork_targets(&agents, "claude", None)),
            vec![
                ("claude".into(), Some("sonnet".into())),
                ("claude".into(), Some("haiku".into())),
            ],
            "no recorded model means the provider's default answered"
        );
    }

    #[test]
    fn another_ready_provider_is_preferred_over_another_model() {
        let agents = [
            with_models(
                agent("claude", true, Some(true)),
                &[("opus", true), ("sonnet", false)],
            ),
            agent("codex", true, None),
        ];
        assert_eq!(
            labels(&model_fork_targets(&agents, "claude", Some("opus"))),
            vec![("codex".into(), None)],
            "a different vendor is the stronger second opinion, and the menu stays short"
        );
    }

    #[test]
    fn a_lone_provider_with_one_model_has_nothing_to_offer() {
        let agents = [with_models(
            agent("claude", true, Some(true)),
            &[("opus", true)],
        )];
        assert!(model_fork_targets(&agents, "claude", Some("opus")).is_empty());
        let signed_out = [with_models(
            agent("claude", true, Some(false)),
            &[("opus", true), ("sonnet", false)],
        )];
        assert!(model_fork_targets(&signed_out, "claude", Some("opus")).is_empty());
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
