//! Whether a newer agent CLI is out — MonoCode's update check, on request.
//!
//! Each supported CLI is published to npm, so its latest version is one
//! small JSON document from the registry. The check runs only when the
//! reader asks (Ginka is local-first and does not phone home), compares the
//! installed version the probe already read, and answers with the command
//! that would update it. Ginka never runs an installer itself: what changes
//! a user's toolchain is theirs to run.

use ginka_protocol::model::{AgentStatus, AgentUpdate};

/// The npm package and the update command for each agent Ginka drives.
pub const PACKAGES: &[(&str, &str, &str)] = &[
    (
        "claude",
        "@anthropic-ai/claude-code",
        "npm install -g @anthropic-ai/claude-code@latest",
    ),
    (
        "codex",
        "@openai/codex",
        "npm install -g @openai/codex@latest",
    ),
    (
        "gemini",
        "@google/gemini-cli",
        "npm install -g @google/gemini-cli@latest",
    ),
    (
        "opencode",
        "opencode-ai",
        "npm install -g opencode-ai@latest",
    ),
];

/// Where a package's latest published version is read.
pub fn registry_url(package: &str) -> String {
    format!("https://registry.npmjs.org/{package}/latest")
}

/// The `version` of a registry `latest` document.
pub fn latest_from_registry(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value["version"]
        .as_str()
        .map(str::to_string)
        .filter(|version| !version.is_empty())
}

/// Whether `latest` is a newer release than `installed`, comparing the dotted
/// numbers and ignoring a pre-release or build suffix. Either one unreadable
/// is not newer — a check that cannot tell must not nag.
pub fn newer(installed: &str, latest: &str) -> bool {
    let numbers = |version: &str| -> Option<Vec<u64>> {
        version
            .trim()
            .trim_start_matches('v')
            .split(['-', '+'])
            .next()?
            .split('.')
            .map(|part| part.parse().ok())
            .collect()
    };
    match (numbers(installed), numbers(latest)) {
        (Some(mut have), Some(mut want)) => {
            let width = have.len().max(want.len());
            have.resize(width, 0);
            want.resize(width, 0);
            want > have
        }
        _ => false,
    }
}

/// Check every installed agent Ginka knows a package for. `fetch` reads a
/// URL's body; a failure leaves that agent's `latest` unknown rather than
/// failing the rest.
pub fn check(
    agents: &[AgentStatus],
    fetch: impl Fn(&str) -> anyhow::Result<String>,
) -> Vec<AgentUpdate> {
    agents
        .iter()
        .filter(|agent| agent.installed)
        .filter_map(|agent| {
            let (_, package, command) = PACKAGES.iter().find(|(id, _, _)| *id == agent.id)?;
            let latest = fetch(&registry_url(package))
                .ok()
                .and_then(|body| latest_from_registry(&body));
            let update_available = match (&agent.version, &latest) {
                (Some(installed), Some(latest)) => newer(installed, latest),
                _ => false,
            };
            Some(AgentUpdate {
                agent: agent.id.clone(),
                installed: agent.version.clone(),
                latest,
                update_available,
                command: command.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(newer("2.1.9", "2.1.10"), "numerically, not as text");
        assert!(newer("0.44.0", "0.45.0"));
        assert!(newer("v1.2", "1.2.1"));
        assert!(!newer("2.1.10", "2.1.10"));
        assert!(!newer("2.2.0", "2.1.99"));
        assert!(
            !newer("2.1.0-beta.1", "2.1.0"),
            "a pre-release counts as its release"
        );
        assert!(!newer("unknown", "2.0.0"));
        assert!(!newer("2.0.0", "next"));
    }

    #[test]
    fn the_registry_answer_and_the_installed_version_make_the_verdict() {
        assert_eq!(
            latest_from_registry(r#"{"name":"x","version":"2.3.0"}"#).as_deref(),
            Some("2.3.0")
        );
        assert_eq!(latest_from_registry("not json"), None);

        let agent = |id: &str, version: Option<&str>, installed: bool| AgentStatus {
            id: id.into(),
            display_name: id.into(),
            program: id.into(),
            installed,
            version: version.map(str::to_string),
            authenticated: None,
            detail: None,
            models: Vec::new(),
        };
        let agents = [
            agent("claude", Some("2.1.0"), true),
            agent("codex", Some("0.50.0"), true),
            agent("gemini", None, false),
            agent("mystery", Some("1.0.0"), true),
        ];
        let updates = check(&agents, |url| {
            if url.contains("claude-code") {
                Ok(r#"{"version":"2.2.0"}"#.into())
            } else {
                anyhow::bail!("offline")
            }
        });
        assert_eq!(
            updates.len(),
            2,
            "not installed and unknown agents are left out"
        );
        assert_eq!(updates[0].agent, "claude");
        assert!(updates[0].update_available);
        assert_eq!(updates[0].latest.as_deref(), Some("2.2.0"));
        assert!(updates[0].command.contains("@anthropic-ai/claude-code"));
        assert_eq!(updates[1].agent, "codex");
        assert!(!updates[1].update_available, "a failed read is not news");
        assert_eq!(updates[1].latest, None);
    }
}
