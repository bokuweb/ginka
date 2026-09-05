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
