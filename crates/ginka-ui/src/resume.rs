//! Resuming a conversation started in an agent's own CLI (`docs/ui.md` §3.3).
//!
//! The daemon lists what `claude` and `codex` kept for the workspace's
//! directory (`ginka_core::cli_sessions`); this is what the dialog does with
//! that list: narrow it as the reader types, and say what each row is.

use crate::workspace::relative_age;
use ginka_protocol::model::CliSession;

/// The rows whose title or agent contains every word typed, in the order
/// the daemon gave them (newest first).
pub fn filter(sessions: &[CliSession], query: &str) -> Vec<CliSession> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    sessions
        .iter()
        .filter(|session| {
            let haystack =
                format!("{} {}", session.title, agent_name(&session.agent)).to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .cloned()
        .collect()
}

/// What the CLI is called, as its users know it.
pub fn agent_name(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        other => other,
    }
}

/// The line under a row's title: which CLI, how long, how recent.
pub fn detail(session: &CliSession, now: i64) -> String {
    rust_i18n::t!(
        "resume.detail",
        agent = agent_name(&session.agent),
        prompts = session.prompts,
        age = relative_age(now, session.updated_at)
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(agent: &str, title: &str) -> CliSession {
        CliSession {
            agent: agent.into(),
            vendor_session_id: format!("{agent}-{title}"),
            title: title.into(),
            prompts: 3,
            updated_at: 1_000,
        }
    }

    #[test]
    fn every_word_typed_must_match_the_title_or_the_cli() {
        let all = [
            session("claude", "Fix the login bug"),
            session("codex", "Add dark mode"),
            session("claude", "Dark mode polish"),
        ];
        let titles = |query: &str| {
            filter(&all, query)
                .into_iter()
                .map(|session| session.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            titles(""),
            ["Fix the login bug", "Add dark mode", "Dark mode polish"]
        );
        assert_eq!(titles("DARK"), ["Add dark mode", "Dark mode polish"]);
        assert_eq!(titles("dark codex"), ["Add dark mode"]);
        assert_eq!(titles("claude code login"), ["Fix the login bug"]);
    }

    #[test]
    fn a_row_says_which_cli_how_many_prompts_and_how_long_ago() {
        rust_i18n::set_locale("en");
        assert_eq!(
            detail(&session("codex", "x"), 1_000 + 7_200),
            "Codex · 3 prompts · 2h"
        );
    }
}
