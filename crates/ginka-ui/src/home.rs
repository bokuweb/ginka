//! The home screen: what the centre column offers before there is a
//! conversation in it.
//!
//! A window with nothing selected is the ordinary way this app opens — the
//! first run, and every "new chat" after it — so the empty centre column is a
//! front door rather than an error state. It asks one question, names the
//! project the answer would run in, and offers four ways to start typing.
//!
//! The strings and the starter prompts live here rather than in the view so
//! they can be tested and translated in one place (`AGENTS.md` rule 8).

use crate::assets::icon;
use ginka_protocol::model::AgentStatus;
use gpui::SharedString;

/// One of the four ways in the home screen offers.
///
/// Choosing one fills the composer rather than sending it: the starter is the
/// first half of a sentence the reader finishes, and a prompt that sent itself
/// would start an agent on a question nobody asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Starter {
    /// Stable across a language change, so a view can key an element by it.
    pub id: &'static str,
    /// The mark on the card, as [`gpui_component::Icon::path`] wants it.
    pub icon: &'static str,
    /// What the card says.
    pub label: SharedString,
    /// What it puts in the composer.
    pub prompt: SharedString,
}

/// What still stands between a first run and a first prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStep {
    /// None of the agents Ginka drives is installed on this machine.
    InstallAgent {
        /// What the agents are called, in offering order.
        agents: Vec<String>,
    },
    /// Agents are installed but none is signed in: these ones say so.
    SignIn {
        /// `(driver id, display name)` of each one signed out.
        agents: Vec<(String, String)>,
    },
    /// No project is registered yet.
    AddProject,
}

/// The steps still to take, in order: an agent that can run, then a project
/// to run it in. Empty once both are there — the home screen is then only
/// the question and the starters. Agents not yet probed say nothing, so the
/// card does not flash up on every launch.
pub fn setup_steps(agents: &[AgentStatus], projects: usize) -> Vec<SetupStep> {
    let mut steps = Vec::new();
    if !agents.is_empty() && !agents.iter().any(AgentStatus::is_ready) {
        let signed_out: Vec<(String, String)> = agents
            .iter()
            .filter(|agent| agent.installed && agent.authenticated == Some(false))
            .map(|agent| (agent.id.clone(), agent.display_name.clone()))
            .collect();
        steps.push(if signed_out.is_empty() {
            SetupStep::InstallAgent {
                agents: agents
                    .iter()
                    .map(|agent| agent.display_name.clone())
                    .collect(),
            }
        } else {
            SetupStep::SignIn { agents: signed_out }
        });
    }
    if projects == 0 {
        steps.push(SetupStep::AddProject);
    }
    steps
}

/// The four starters, in the order they are drawn.
///
/// Explore, build, review, fix: the four things a coding agent is asked for
/// first, and between them they say what this window is for to a reader who
/// has never used it.
pub fn starters() -> Vec<Starter> {
    // Keys are literals rather than built from `id`: `t!` checks them against
    // `locales/app.yml` at compile time, and a key assembled at run time is
    // one a missing translation only reports in a running window.
    vec![
        Starter {
            id: "explore",
            icon: icon::COMPASS,
            label: rust_i18n::t!("home.starter.explore").to_string().into(),
            prompt: rust_i18n::t!("home.starter.explore.prompt")
                .to_string()
                .into(),
        },
        Starter {
            id: "build",
            icon: icon::HAMMER,
            label: rust_i18n::t!("home.starter.build").to_string().into(),
            prompt: rust_i18n::t!("home.starter.build.prompt")
                .to_string()
                .into(),
        },
        Starter {
            id: "review",
            icon: icon::LIST_CHECK,
            label: rust_i18n::t!("home.starter.review").to_string().into(),
            prompt: rust_i18n::t!("home.starter.review.prompt")
                .to_string()
                .into(),
        },
        Starter {
            id: "fix",
            icon: icon::BUG,
            label: rust_i18n::t!("home.starter.fix").to_string().into(),
            prompt: rust_i18n::t!("home.starter.fix.prompt").to_string().into(),
        },
    ]
}

/// What the home screen asks.
///
/// With a project chosen it says so, because "what shall we build" and "what
/// shall we build *in totoro*" are different questions and the second is the
/// one the window is about to act on. Without one it stays general: a chat
/// with no project is a real thing here (it runs in a scratch worktree), not a
/// state to apologise for.
pub fn greeting(project: Option<&str>) -> String {
    match project {
        Some(project) => rust_i18n::t!("home.greeting.project", project = project).to_string(),
        None => rust_i18n::t!("home.greeting").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_starter_carries_a_prompt_and_a_mark() {
        let starters = starters();
        assert_eq!(starters.len(), 4);
        for starter in &starters {
            assert!(!starter.label.is_empty(), "{} has no label", starter.id);
            assert!(
                !starter.prompt.is_empty(),
                "{} would fill the composer with nothing",
                starter.id
            );
            assert!(starter.icon.ends_with(".svg"));
        }
    }

    #[test]
    fn the_greeting_names_the_project_the_answer_would_run_in() {
        assert!(greeting(Some("totoro")).contains("totoro"));
        assert!(!greeting(None).contains("totoro"));
    }

    #[test]
    fn the_home_screen_speaks_the_user_s_language() {
        // The locale is one global for the process, so this test puts it back.
        ginka_core::i18n::apply("ja");
        let japanese = greeting(None);
        let starters = starters();
        ginka_core::i18n::apply("en");
        assert_ne!(japanese, greeting(None));
        assert_ne!(starters[0].label, self::starters()[0].label);
    }
}

#[cfg(test)]
mod setup_tests {
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
    fn nothing_is_asked_before_the_agents_are_known() {
        assert_eq!(setup_steps(&[], 1), []);
        assert_eq!(setup_steps(&[], 0), [SetupStep::AddProject]);
    }

    #[test]
    fn with_nothing_installed_the_first_step_is_an_agent() {
        let agents = [agent("claude", false, None), agent("codex", false, None)];
        assert_eq!(
            setup_steps(&agents, 0),
            [
                SetupStep::InstallAgent {
                    agents: vec!["CLAUDE".into(), "CODEX".into()]
                },
                SetupStep::AddProject
            ]
        );
    }

    #[test]
    fn installed_but_signed_out_asks_for_a_sign_in_to_those_only() {
        let agents = [
            agent("claude", true, Some(false)),
            agent("codex", false, None),
        ];
        assert_eq!(
            setup_steps(&agents, 1),
            [SetupStep::SignIn {
                agents: vec![("claude".into(), "CLAUDE".into())]
            }]
        );
    }

    #[test]
    fn one_ready_agent_and_a_project_is_ready() {
        let agents = [
            agent("claude", true, Some(false)),
            agent("codex", true, None),
        ];
        assert_eq!(setup_steps(&agents, 2), []);
    }
}
