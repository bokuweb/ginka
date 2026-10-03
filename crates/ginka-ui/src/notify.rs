//! Desktop notifications: when an agent is worth interrupting the reader for.
//!
//! Orca and MonoCode both tell the desktop when an agent finishes or stops to
//! ask something. The rule is what makes that useful rather than noise: a
//! notification is for a change the reader has not seen — a session moving
//! from working to done, failed or waiting — and only while the window is not
//! the one in front. Anything else they are already looking at.

use ginka_protocol::model::SessionState;

/// Why the reader is being told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notable {
    /// The turn ended and the agent is waiting for the next prompt.
    Finished,
    /// The agent asked a question or proposed a plan.
    NeedsYou,
    /// The turn failed.
    Failed,
}

impl Notable {
    /// The notification's heading, in the reader's language.
    pub fn heading(self) -> String {
        match self {
            Self::Finished => rust_i18n::t!("notify.finished"),
            Self::NeedsYou => rust_i18n::t!("notify.needs_you"),
            Self::Failed => rust_i18n::t!("notify.failed"),
        }
        .to_string()
    }
}

/// Whether a move from `before` to `after` is worth a notification.
///
/// Only a move out of work: a session that was already idle and is re-read
/// as idle has nothing new to say, and a cancel is something the reader did.
pub fn notable(before: Option<SessionState>, after: SessionState) -> Option<Notable> {
    let was_working = matches!(before, Some(SessionState::Starting | SessionState::Running));
    if !was_working {
        return None;
    }
    match after {
        SessionState::Idle | SessionState::Finished => Some(Notable::Finished),
        SessionState::AwaitingInput => Some(Notable::NeedsYou),
        SessionState::Failed => Some(Notable::Failed),
        SessionState::Starting | SessionState::Running | SessionState::Cancelled => None,
    }
}

impl Notable {
    /// The system sound it plays: a bright one for done, a question's ping,
    /// and a low one for a failure — told apart without looking.
    pub fn sound(self) -> &'static str {
        match self {
            Self::Finished => "Glass",
            Self::NeedsYou => "Ping",
            Self::Failed => "Basso",
        }
    }
}

/// [`applescript`], playing `sound` as it shows; silent without one.
pub fn applescript_with_sound(title: &str, body: &str, sound: Option<&str>) -> String {
    let script = applescript(title, body);
    match sound {
        Some(sound) => format!("{script} sound name {}", quote(sound)),
        None => script,
    }
}

/// The Dock badge: how many sessions are waiting on the reader — asked
/// something or failed — or none. Work in progress is not counted; it needs
/// nobody yet.
pub fn badge(states: impl IntoIterator<Item = crate::workspace::AgentState>) -> Option<String> {
    let waiting = states
        .into_iter()
        .filter(|state| *state == crate::workspace::AgentState::NeedsAttention)
        .count();
    match waiting {
        0 => None,
        1..=99 => Some(waiting.to_string()),
        _ => Some("99+".to_string()),
    }
}

/// The AppleScript that shows a notification, with both strings quoted so
/// nothing in a session title can end the string early.
pub fn applescript(title: &str, body: &str) -> String {
    format!(
        "display notification {} with title {}",
        quote(body),
        quote(title)
    )
}

/// An AppleScript string literal. Newlines become spaces: a notification is
/// one line, and a raw newline would end the statement.
fn quote(text: &str) -> String {
    let escaped: String = text
        .chars()
        .map(|c| match c {
            '\n' | '\r' => ' '.to_string(),
            '\\' => "\\\\".to_string(),
            '"' => "\\\"".to_string(),
            c => c.to_string(),
        })
        .collect();
    format!("\"{escaped}\"")
}

/// How long a project's notifications are muted for — MonoCode's per-project
/// mute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuteFor {
    /// This many hours from now.
    Hours(u32),
    /// Until the reader unmutes it.
    UntilResumed,
}

impl MuteFor {
    /// The choices a project's menu offers, in order.
    pub const CHOICES: [MuteFor; 4] = [
        MuteFor::Hours(1),
        MuteFor::Hours(4),
        MuteFor::Hours(8),
        MuteFor::UntilResumed,
    ];

    /// When a mute chosen at `now` ends, in Unix seconds; `i64::MAX` for
    /// one that lasts until it is lifted.
    pub fn until(self, now: i64) -> i64 {
        match self {
            Self::Hours(hours) => now.saturating_add(i64::from(hours) * 3_600),
            Self::UntilResumed => i64::MAX,
        }
    }
}

/// Whether `project`'s notifications are muted at `now`. A mute that has
/// run out is not one, so nothing has to come along and clear it.
pub fn muted(mutes: &std::collections::BTreeMap<String, i64>, project: &str, now: i64) -> bool {
    mutes.get(project).is_some_and(|until| now < *until)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_muted_project_is_quiet_until_its_mute_runs_out() {
        let now = 1_000;
        let mut mutes = std::collections::BTreeMap::new();
        mutes.insert("comet".to_string(), MuteFor::Hours(1).until(now));
        mutes.insert("forever".to_string(), MuteFor::UntilResumed.until(now));
        assert!(muted(&mutes, "comet", now));
        assert!(muted(&mutes, "comet", now + 3_599));
        assert!(
            !muted(&mutes, "comet", now + 3_600),
            "an hour later it speaks again"
        );
        assert!(muted(&mutes, "forever", i64::MAX - 1));
        assert!(
            !muted(&mutes, "other", now),
            "only the project that was muted"
        );
        assert_eq!(MuteFor::Hours(8).until(0), 8 * 3_600);
    }

    use super::*;

    #[test]
    fn only_a_move_out_of_work_is_worth_telling() {
        use SessionState::*;
        assert_eq!(notable(Some(Running), Idle), Some(Notable::Finished));
        assert_eq!(
            notable(Some(Running), AwaitingInput),
            Some(Notable::NeedsYou)
        );
        assert_eq!(notable(Some(Starting), Failed), Some(Notable::Failed));
        assert_eq!(
            notable(Some(Running), Cancelled),
            None,
            "the reader did that"
        );
        assert_eq!(notable(Some(Idle), Idle), None, "nothing new");
        assert_eq!(notable(None, Finished), None, "a first reading is not news");
    }

    #[test]
    fn each_kind_of_news_has_its_own_sound_and_silence_is_a_choice() {
        assert_eq!(Notable::Finished.sound(), "Glass");
        assert_eq!(Notable::NeedsYou.sound(), "Ping");
        assert_eq!(Notable::Failed.sound(), "Basso");
        assert_eq!(
            applescript_with_sound("Done", "Fix login", Some("Glass")),
            "display notification \"Fix login\" with title \"Done\" sound name \"Glass\""
        );
        assert_eq!(
            applescript_with_sound("Done", "Fix login", None),
            applescript("Done", "Fix login")
        );
    }

    #[test]
    fn the_dock_counts_what_is_waiting_on_the_reader() {
        use crate::workspace::AgentState::*;
        assert_eq!(badge([Idle, Working, Idle]), None, "nothing waits");
        assert_eq!(
            badge([NeedsAttention, Working, NeedsAttention]).as_deref(),
            Some("2")
        );
        assert_eq!(
            badge(std::iter::repeat_n(NeedsAttention, 120)).as_deref(),
            Some("99+"),
            "a badge has room for two digits"
        );
    }

    #[test]
    fn a_title_cannot_break_out_of_the_script() {
        let script = applescript("Fix \"quotes\"", "line one\nline \\two");
        assert_eq!(
            script,
            "display notification \"line one line \\\\two\" with title \"Fix \\\"quotes\\\"\""
        );
    }
}
