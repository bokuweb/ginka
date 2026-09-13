//! Model-picker decisions shared by the composer view.

use ginka_protocol::model::AgentStatus;
use ginka_protocol::provider::ProviderModel;

/// Providers that can currently run and advertise at least one model.
pub fn available_providers(agents: &[AgentStatus]) -> Vec<&AgentStatus> {
    agents
        .iter()
        .filter(|agent| agent.installed)
        .filter(|agent| agent.authenticated != Some(false))
        .filter(|agent| !agent.models.is_empty())
        .collect()
}

/// Models from one CLI matching a case-insensitive id or display-name query.
pub fn matching_models(agent: &AgentStatus, query: &str) -> Vec<ProviderModel> {
    let query = query.trim().to_lowercase();
    agent
        .models
        .iter()
        .filter(|model| {
            query.is_empty()
                || model.id.to_lowercase().contains(&query)
                || model.label.to_lowercase().contains(&query)
        })
        .cloned()
        .collect()
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
    fn only_runnable_cli_catalogues_become_provider_tabs() {
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
            ["claude"]
        );
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
        assert_eq!(matching_models(&claude, "unknown").len(), 0);
        assert_eq!(matching_models(&claude, "").len(), 2);
    }
}
