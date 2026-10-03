//! What a message from a thread becomes.
//!
//! Every rule in `docs/connectors.md` §5 is a branch here and a test below.
//! Nothing in this module talks to a platform or a database: what it needs
//! to know about the world arrives through [`Lookup`], so the awkward cases
//! — a verdict from someone who may not approve, a mention into a thread the
//! bot has never seen — are pinned without a network.

use super::config::{SlackSettings, Trigger};
use super::text::{Answer, Control, parse_control, parse_verdict};
use ginka_protocol::SessionId;
use ginka_protocol::model::SessionOrigin;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// A file on an inbound message, before it is fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundFile {
    /// The platform's own file id.
    pub id: String,
    /// The name the sender knows it by. Display only.
    pub name: String,
    /// Where the bytes are, for the adapter to fetch with its token.
    pub url: String,
}

/// A message a transport received, reduced to what the policy needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbound {
    /// `slack`.
    pub connector: String,
    /// The conversation id.
    pub channel: String,
    /// The root message's key when this is a reply; this message's own key
    /// when it is a root.
    pub thread: String,
    /// This message's own key.
    pub message: String,
    /// The platform's member id of the sender.
    pub sender: String,
    /// What was written, mention stripped and entities unescaped.
    pub text: String,
    /// Whether the bot was mentioned.
    pub mentions_bot: bool,
    /// Whether this is a root message rather than a reply in a thread.
    pub is_root: bool,
    /// Attachments, not yet fetched; a message with files and no text is not empty.
    pub files: Vec<InboundFile>,
    /// Whether this is an edit of a message rather than a new one.
    pub is_edit: bool,
    /// Whether the bot itself, or any bot, wrote it.
    pub from_bot: bool,
}

impl Inbound {
    /// Where a session started from this message would say it came from.
    pub fn origin(&self) -> SessionOrigin {
        SessionOrigin {
            connector: self.connector.clone(),
            channel: self.channel.clone(),
            thread: self.thread.clone(),
        }
    }
}

/// Why a message was ignored. Logged at debug, never answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreReason {
    /// The bot's own posts, and other bots'.
    FromBot,
    /// An edit: a turn already ran on the original text.
    Edit,
    /// A channel with no binding.
    Unbound,
    /// The sender is not on the allowlist. The room is never a grant.
    NotAllowed,
    /// Socket Mode redelivered it.
    Duplicate,
    /// Nothing to send: no text and no files.
    Empty,
    /// A root message, or a reply in a thread the bot does not own, with no
    /// mention where the binding wants one.
    NoMention,
    /// A verdict from someone who may ask but not approve.
    NotApprover,
    /// A verdict naming a request that is not open.
    UnknownRequest,
}

/// What the connector does with a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Drop it silently; the reason is only logged.
    Ignore(IgnoreReason),
    /// Start a session in the binding's target.
    Start {
        /// Index into the settings' bindings.
        binding: usize,
        /// Whether the thread so far should be quoted into the prompt: the
        /// bot was mentioned into a thread it had never seen.
        quote_thread: bool,
    },
    /// Send a follow-up into the session already answering this thread.
    Continue {
        /// The session already mapped to this thread.
        session: SessionId,
    },
    /// A word for the connector rather than the agent.
    Control {
        /// The session mapped to this thread.
        session: SessionId,
        /// The control word, which only counts when it is the whole message.
        command: Control,
    },
    /// An answer to a question the agent asked.
    Verdict {
        /// The session that asked.
        session: SessionId,
        /// The open request the verdict names, lowercased.
        request_id: String,
        /// What to send back to the agent.
        answer: Answer,
    },
}

/// What the policy needs to know about the world.
pub trait Lookup {
    /// The session still answering this thread, if any.
    fn session_for(&self, origin: &SessionOrigin) -> Option<SessionId>;
    /// The request ids the agent is waiting on in a session.
    fn open_requests(&self, session: &SessionId) -> Vec<String>;
    /// Whether this message has already been handled.
    fn seen(&self, channel: &str, message: &str) -> bool;
}

/// Decide what a message becomes.
pub fn decide(inbound: &Inbound, settings: &SlackSettings, lookup: &dyn Lookup) -> Decision {
    use Decision::Ignore;
    if inbound.from_bot {
        return Ignore(IgnoreReason::FromBot);
    }
    if inbound.is_edit {
        return Ignore(IgnoreReason::Edit);
    }
    let Some((binding_index, binding)) = settings.binding_for(&inbound.channel) else {
        return Ignore(IgnoreReason::Unbound);
    };
    // The sender, never the room: being in a bound channel is not a grant.
    if !settings.is_allowed(&inbound.sender) {
        return Ignore(IgnoreReason::NotAllowed);
    }
    if lookup.seen(&inbound.channel, &inbound.message) {
        return Ignore(IgnoreReason::Duplicate);
    }
    if inbound.text.trim().is_empty() && inbound.files.is_empty() {
        return Ignore(IgnoreReason::Empty);
    }

    if let Some(session) = lookup.session_for(&inbound.origin()) {
        if let Some(command) = parse_control(&inbound.text) {
            return Decision::Control { session, command };
        }
        if let Some((request_id, answer)) = parse_verdict(&inbound.text) {
            if !lookup.open_requests(&session).contains(&request_id) {
                return Ignore(IgnoreReason::UnknownRequest);
            }
            if !settings.may_approve(&inbound.sender) {
                return Ignore(IgnoreReason::NotApprover);
            }
            return Decision::Verdict {
                session,
                request_id,
                answer,
            };
        }
        // Mention or not: a reply in a thread the bot owns is for the bot.
        return Decision::Continue { session };
    }

    if inbound.is_root {
        if binding.trigger == Trigger::Mention && !inbound.mentions_bot {
            return Ignore(IgnoreReason::NoMention);
        }
        return Decision::Start {
            binding: binding_index,
            quote_thread: false,
        };
    }
    // A reply in a thread the bot has never seen: only a mention starts
    // something, whatever the binding's trigger, because `all` is about root
    // messages and every reply in every thread is not what anyone meant.
    if !inbound.mentions_bot {
        return Ignore(IgnoreReason::NoMention);
    }
    Decision::Start {
        binding: binding_index,
        quote_thread: true,
    }
}

/// A per-sender budget of turns per hour.
///
/// A sliding window rather than a calendar hour: thirty asks at 9:59 and
/// thirty more at 10:00 is the case a calendar hour lets through.
#[derive(Debug, Default)]
pub struct RateLimiter {
    starts: HashMap<String, VecDeque<i64>>,
}

/// One hour, in the seconds the limiter counts in.
const WINDOW_SECS: i64 = 3_600;

impl RateLimiter {
    /// Whether `sender` may start another turn at `now`, recording it if so.
    pub fn admit(&mut self, sender: &str, now: i64, limit: u32) -> bool {
        let starts = self.starts.entry(sender.to_string()).or_default();
        while starts.front().is_some_and(|at| now - *at >= WINDOW_SECS) {
            starts.pop_front();
        }
        if starts.len() >= limit as usize {
            return false;
        }
        starts.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::config::{Binding, WorktreeMode};
    use ginka_protocol::ProjectName;
    use std::collections::HashSet;

    #[derive(Default)]
    struct World {
        sessions: HashMap<SessionOrigin, SessionId>,
        open: HashMap<SessionId, Vec<String>>,
        seen: HashSet<(String, String)>,
    }

    impl Lookup for World {
        fn session_for(&self, origin: &SessionOrigin) -> Option<SessionId> {
            self.sessions.get(origin).cloned()
        }
        fn open_requests(&self, session: &SessionId) -> Vec<String> {
            self.open.get(session).cloned().unwrap_or_default()
        }
        fn seen(&self, channel: &str, message: &str) -> bool {
            self.seen
                .contains(&(channel.to_string(), message.to_string()))
        }
    }

    fn settings() -> SlackSettings {
        SlackSettings {
            allowed_users: vec!["UALICE".into(), "UBOB".into()],
            approvers: Some(vec!["UALICE".into()]),
            bindings: vec![
                Binding {
                    channel: "CBOUND".into(),
                    project: Some(ProjectName("comet".into())),
                    ..Binding::default()
                },
                Binding {
                    channel: "CALL".into(),
                    project: Some(ProjectName("comet".into())),
                    trigger: Trigger::All,
                    worktree: WorktreeMode::PerThread,
                    max_concurrent: 3,
                    ..Binding::default()
                },
            ],
            ..SlackSettings::default()
        }
    }

    fn root(channel: &str, text: &str, mentions: bool) -> Inbound {
        Inbound {
            connector: "slack".into(),
            channel: channel.into(),
            thread: "1.0".into(),
            message: "1.0".into(),
            sender: "UALICE".into(),
            text: text.into(),
            mentions_bot: mentions,
            is_root: true,
            files: Vec::new(),
            is_edit: false,
            from_bot: false,
        }
    }

    fn reply(channel: &str, text: &str, mentions: bool) -> Inbound {
        Inbound {
            message: "1.5".into(),
            is_root: false,
            mentions_bot: mentions,
            text: text.into(),
            ..root(channel, text, mentions)
        }
    }

    fn owned() -> World {
        let mut world = World::default();
        world.sessions.insert(
            SessionOrigin {
                connector: "slack".into(),
                channel: "CBOUND".into(),
                thread: "1.0".into(),
            },
            SessionId("s-1".into()),
        );
        world
            .open
            .insert(SessionId("s-1".into()), vec!["abcde".into()]);
        world
    }

    #[test]
    fn a_mention_on_a_root_message_starts_a_session() {
        assert_eq!(
            decide(
                &root("CBOUND", "fix it", true),
                &settings(),
                &World::default()
            ),
            Decision::Start {
                binding: 0,
                quote_thread: false
            }
        );
    }

    #[test]
    fn a_root_message_without_a_mention_is_ignored_unless_the_binding_takes_all() {
        assert_eq!(
            decide(
                &root("CBOUND", "just chatting", false),
                &settings(),
                &World::default()
            ),
            Decision::Ignore(IgnoreReason::NoMention)
        );
        assert_eq!(
            decide(
                &root("CALL", "just chatting", false),
                &settings(),
                &World::default()
            ),
            Decision::Start {
                binding: 1,
                quote_thread: false
            }
        );
    }

    #[test]
    fn the_sender_is_gated_never_the_room() {
        let mut stranger = root("CBOUND", "fix it", true);
        stranger.sender = "UEVE".into();
        assert_eq!(
            decide(&stranger, &settings(), &World::default()),
            Decision::Ignore(IgnoreReason::NotAllowed)
        );
        // An allowed sender in an unbound channel is not answered either.
        assert_eq!(
            decide(
                &root("CELSEWHERE", "fix it", true),
                &settings(),
                &World::default()
            ),
            Decision::Ignore(IgnoreReason::Unbound)
        );
    }

    #[test]
    fn bots_edits_duplicates_and_empty_messages_are_dropped() {
        let mut bot = root("CBOUND", "fix it", true);
        bot.from_bot = true;
        assert_eq!(
            decide(&bot, &settings(), &World::default()),
            Decision::Ignore(IgnoreReason::FromBot)
        );
        let mut edit = root("CBOUND", "fix it", true);
        edit.is_edit = true;
        assert_eq!(
            decide(&edit, &settings(), &World::default()),
            Decision::Ignore(IgnoreReason::Edit)
        );
        let mut world = World::default();
        world.seen.insert(("CBOUND".into(), "1.0".into()));
        assert_eq!(
            decide(&root("CBOUND", "fix it", true), &settings(), &world),
            Decision::Ignore(IgnoreReason::Duplicate)
        );
        assert_eq!(
            decide(&root("CBOUND", "   ", true), &settings(), &World::default()),
            Decision::Ignore(IgnoreReason::Empty)
        );
    }

    #[test]
    fn a_file_with_no_text_is_still_a_message() {
        let mut with_file = root("CBOUND", "", true);
        with_file.files.push(InboundFile {
            id: "F1".into(),
            name: "log.txt".into(),
            url: "https://files/F1".into(),
        });
        assert!(matches!(
            decide(&with_file, &settings(), &World::default()),
            Decision::Start { .. }
        ));
    }

    #[test]
    fn a_reply_in_an_owned_thread_continues_mention_or_not() {
        assert_eq!(
            decide(
                &reply("CBOUND", "also check the tests", false),
                &settings(),
                &owned()
            ),
            Decision::Continue {
                session: SessionId("s-1".into())
            }
        );
    }

    #[test]
    fn a_mention_into_a_thread_the_bot_never_saw_starts_with_the_thread_quoted() {
        assert_eq!(
            decide(
                &reply("CBOUND", "fix this", true),
                &settings(),
                &World::default()
            ),
            Decision::Start {
                binding: 0,
                quote_thread: true
            }
        );
        // Without a mention it is someone else's conversation, even where the
        // binding takes every root message.
        assert_eq!(
            decide(&reply("CALL", "hmm", false), &settings(), &World::default()),
            Decision::Ignore(IgnoreReason::NoMention)
        );
    }

    #[test]
    fn a_control_word_is_for_the_connector_and_anything_longer_is_a_prompt() {
        assert_eq!(
            decide(&reply("CBOUND", "stop", false), &settings(), &owned()),
            Decision::Control {
                session: SessionId("s-1".into()),
                command: Control::Stop
            }
        );
        assert!(matches!(
            decide(
                &reply("CBOUND", "stop touching main.rs", false),
                &settings(),
                &owned()
            ),
            Decision::Continue { .. }
        ));
        // A control word in a thread the bot does not own is just a word.
        assert_eq!(
            decide(
                &reply("CBOUND", "stop", false),
                &settings(),
                &World::default()
            ),
            Decision::Ignore(IgnoreReason::NoMention)
        );
    }

    #[test]
    fn a_verdict_needs_an_open_request_and_an_approver() {
        assert_eq!(
            decide(&reply("CBOUND", "yes abcde", false), &settings(), &owned()),
            Decision::Verdict {
                session: SessionId("s-1".into()),
                request_id: "abcde".into(),
                answer: Answer::Yes
            }
        );
        let mut bob = reply("CBOUND", "yes abcde", false);
        bob.sender = "UBOB".into();
        assert_eq!(
            decide(&bob, &settings(), &owned()),
            Decision::Ignore(IgnoreReason::NotApprover),
            "bob may ask but not approve"
        );
        assert_eq!(
            decide(&reply("CBOUND", "yes zzzzz", false), &settings(), &owned()),
            Decision::Ignore(IgnoreReason::UnknownRequest)
        );
    }

    #[test]
    fn the_rate_limiter_is_a_sliding_hour() {
        let mut limiter = RateLimiter::default();
        for minute in 0..3 {
            assert!(limiter.admit("UALICE", minute * 60, 3));
        }
        assert!(!limiter.admit("UALICE", 3 * 60, 3), "the fourth in an hour");
        assert!(limiter.admit("UBOB", 3 * 60, 3), "budgets are per sender");
        assert!(
            limiter.admit("UALICE", 3_600, 3),
            "the first ask has aged out of the window"
        );
    }
}
