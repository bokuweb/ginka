//! What an adapter has to provide, and a scripted one for tests.
//!
//! Kept small enough that a scripted transport fits in a test: the adapter
//! owns the platform's API and its rate limits; the connector only ever
//! asks for these seven things.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A reaction the thread sees. Named by meaning; the adapter picks the emoji.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glyph {
    /// The message was accepted and an agent is on it.
    Working,
    /// The turn ended well.
    Done,
    /// The turn failed.
    Failed,
    /// The agent asked something and is waiting.
    Waiting,
    /// The turn was cancelled.
    Stopped,
    /// The message is queued behind the binding's concurrency cap.
    Queued,
}

/// The platform calls a connector makes.
///
/// Every method takes the channel: Slack addresses a message by channel and
/// timestamp together, and an adapter that had to remember which channel a
/// message id belonged to would be keeping state this trait exists to avoid.
pub trait ChatTransport: Send {
    /// Post into a thread. Answers with the new message's id.
    fn post(&mut self, channel: &str, thread: &str, text: &str) -> Result<String>;
    /// Replace a message's text.
    fn edit(&mut self, channel: &str, message: &str, text: &str) -> Result<()>;
    /// Delete a message the bot posted.
    fn delete(&mut self, channel: &str, message: &str) -> Result<()>;
    /// Put a reaction on a message.
    fn react(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()>;
    /// Take a reaction back off.
    fn unreact(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()>;
    /// Attach a file to a thread.
    fn upload(&mut self, channel: &str, thread: &str, name: &str, bytes: &[u8]) -> Result<()>;
}

/// Test doubles for the transport.
pub mod testing {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// One call a scripted transport saw.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Call {
        /// A [`ChatTransport::post`] into a thread.
        Post {
            /// Platform channel id.
            channel: String,
            /// Id of the thread's root message.
            thread: String,
            /// Body as posted, already rendered for the platform.
            text: String,
        },
        /// A [`ChatTransport::edit`] replacing a message's text.
        Edit {
            /// Platform channel id.
            channel: String,
            /// Id of the message edited, as `post` returned it.
            message: String,
            /// The replacement text.
            text: String,
        },
        /// A [`ChatTransport::delete`] of a bot message.
        Delete {
            /// Platform channel id.
            channel: String,
            /// Id of the message deleted.
            message: String,
        },
        /// A [`ChatTransport::react`] adding a reaction.
        React {
            /// Platform channel id.
            channel: String,
            /// Id of the message reacted to.
            message: String,
            /// The reaction, by meaning rather than emoji.
            glyph: Glyph,
        },
        /// A [`ChatTransport::unreact`] taking a reaction off.
        Unreact {
            /// Platform channel id.
            channel: String,
            /// Id of the message the reaction came off.
            message: String,
            /// The reaction removed.
            glyph: Glyph,
        },
        /// A [`ChatTransport::upload`] attaching a file.
        Upload {
            /// Platform channel id.
            channel: String,
            /// Id of the thread's root message.
            thread: String,
            /// File name shown in the thread.
            name: String,
            /// Payload length in bytes; the bytes themselves are not kept.
            bytes: usize,
        },
    }

    /// A transport that records what it was asked and answers with made-up
    /// ids. Never touches a network.
    #[derive(Debug, Default, Clone)]
    pub struct ScriptedTransport {
        calls: Arc<Mutex<Vec<Call>>>,
        next_id: Arc<Mutex<u64>>,
        /// When set, every call fails with this message.
        failing: Option<String>,
    }

    impl ScriptedTransport {
        /// A transport that succeeds at everything and has recorded nothing yet.
        pub fn new() -> Self {
            Self::default()
        }

        /// A transport whose every call fails, for the ledger's sake.
        pub fn failing(message: &str) -> Self {
            Self {
                failing: Some(message.to_string()),
                ..Self::default()
            }
        }

        /// Everything it was asked, in order.
        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        /// The texts of every post, in order.
        pub fn posts(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    Call::Post { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn record(&self, call: Call) -> Result<()> {
            if let Some(message) = &self.failing {
                anyhow::bail!("{message}");
            }
            self.calls.lock().unwrap().push(call);
            Ok(())
        }
    }

    impl ChatTransport for ScriptedTransport {
        fn post(&mut self, channel: &str, thread: &str, text: &str) -> Result<String> {
            self.record(Call::Post {
                channel: channel.into(),
                thread: thread.into(),
                text: text.into(),
            })?;
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            Ok(format!("m{}", *next))
        }

        fn edit(&mut self, channel: &str, message: &str, text: &str) -> Result<()> {
            self.record(Call::Edit {
                channel: channel.into(),
                message: message.into(),
                text: text.into(),
            })
        }

        fn delete(&mut self, channel: &str, message: &str) -> Result<()> {
            self.record(Call::Delete {
                channel: channel.into(),
                message: message.into(),
            })
        }

        fn react(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
            self.record(Call::React {
                channel: channel.into(),
                message: message.into(),
                glyph,
            })
        }

        fn unreact(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
            self.record(Call::Unreact {
                channel: channel.into(),
                message: message.into(),
                glyph,
            })
        }

        fn upload(&mut self, channel: &str, thread: &str, name: &str, bytes: &[u8]) -> Result<()> {
            self.record(Call::Upload {
                channel: channel.into(),
                thread: thread.into(),
                name: name.into(),
                bytes: bytes.len(),
            })
        }
    }
}
