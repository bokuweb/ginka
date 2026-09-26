//! Model-picker decisions shared by the composer view.

use ginka_protocol::model::AgentStatus;
use ginka_protocol::provider::ProviderModel;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// Providers installed here that advertise at least one model.
///
/// A signed-out CLI stays in the list: hiding it made the picker look as if
/// the provider did not exist, when the fix is one `login` away — and the
/// user may be signing in in another window. The panel says so instead
/// (see [`signed_out`]).
pub fn available_providers(agents: &[AgentStatus]) -> Vec<&AgentStatus> {
    agents
        .iter()
        .filter(|agent| agent.installed)
        .filter(|agent| !agent.models.is_empty())
        .collect()
}

/// Whether the CLI said it is not signed in. Unknown is not signed out.
pub fn signed_out(agent: &AgentStatus) -> bool {
    agent.authenticated == Some(false)
}

/// Models from one CLI matching a case-insensitive id or display-name query.
pub fn matching_models(agent: &AgentStatus, query: &str) -> Vec<ProviderModel> {
    let query = query.trim();
    if query.is_empty() {
        return agent.models.clone();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored = agent
        .models
        .iter()
        .filter_map(|model| {
            let score = [&model.id, &model.label]
                .into_iter()
                .filter_map(|field| {
                    let mut matcher = Matcher::new(Config::DEFAULT);
                    let mut buffer = Vec::new();
                    pattern.score(
                        nucleo_matcher::Utf32Str::new(field, &mut buffer),
                        &mut matcher,
                    )
                })
                .max()?;
            Some((score, model.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    scored.into_iter().map(|(_, model)| model).collect()
}

/// Human-readable effort for the selected model, if the id is still valid.
pub fn effort_label<'a>(model: &'a ProviderModel, effort: Option<&str>) -> Option<&'a str> {
    let effort = effort?;
    model
        .reasoning_efforts
        .iter()
        .find(|option| option.id == effort)
        .map(|option| option.label.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, installed: bool, authenticated: Option<bool>) -> AgentStatus {
        AgentStatus {
            id: id.into(),
            display_name: id.into(),
            program: id.into(),
            installed,
            version: None,
            authenticated,
            detail: None,
            models: vec![
                ProviderModel::new("sonnet-4.6", "Sonnet 4.6"),
                ProviderModel::new("opus-4.1", "Opus 4.1"),
            ],
        }
    }

    #[test]
    fn installed_cli_catalogues_become_provider_tabs_even_signed_out() {
        let mut no_models = agent("empty", true, Some(true));
        no_models.models.clear();
        let agents = [
            agent("claude", true, Some(true)),
            agent("signed-out", true, Some(false)),
            agent("missing", false, None),
            no_models,
        ];

        let providers = available_providers(&agents);
        assert_eq!(
            providers
                .iter()
                .map(|agent| agent.id.as_str())
                .collect::<Vec<_>>(),
            ["claude", "signed-out"]
        );
        assert!(!signed_out(providers[0]));
        assert!(signed_out(providers[1]));
    }

    #[test]
    fn search_matches_model_ids_and_labels_without_case() {
        let claude = agent("claude", true, Some(true));
        assert_eq!(
            matching_models(&claude, "SONNET")
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["sonnet-4.6"]
        );
        assert_eq!(matching_models(&claude, "4.1").len(), 1);
        assert_eq!(matching_models(&claude, "snt46")[0].id, "sonnet-4.6");
        assert_eq!(matching_models(&claude, "unknown").len(), 0);
        assert_eq!(matching_models(&claude, "").len(), 2);
    }

    #[test]
    fn the_combined_label_uses_only_an_effort_the_model_advertises() {
        let model = ProviderModel::new("gpt", "GPT").with_reasoning_efforts([
            ginka_protocol::provider::ProviderOption::new("low", "Low"),
            ginka_protocol::provider::ProviderOption::new("high", "High"),
        ]);
        assert_eq!(effort_label(&model, Some("high")), Some("High"));
        assert_eq!(effort_label(&model, Some("xhigh")), None);
        assert_eq!(effort_label(&model, None), None);
    }
}
