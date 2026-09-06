//! Chat connectors: a thread in a chat platform as a way to reach an agent.
//!
//! A connector is a fourth client of the protocol, hosted in the daemon
//! (`docs/connectors.md` §2). What lives here is the part with decisions in
//! it, and none of it touches a network:
//!
//! - [`config`] — the settings shape and what makes it invalid
//! - [`secrets`] — where tokens come from and where they never go
//! - [`decide`] — what an inbound message becomes
//! - [`fold`] — what a thread sees of a turn, from the agent's events
//! - [`deliver`] — those outbounds as calls on a [`transport::ChatTransport`]
//! - [`prompt`] — the one template that attributes a message to its sender
//! - [`text`] — `mrkdwn`, chunking, control words and verdicts
//! - [`ledger`] — the delivery ledger and the dedup set, in SQLite
//!
//! The Slack adapter in `ginka-daemon` implements the transport and feeds
//! [`decide::Inbound`]s in; a scripted transport stands in for it in tests.

pub mod config;
pub mod decide;
pub mod deliver;
pub mod fold;
pub mod ledger;
pub mod prompt;
pub mod secrets;
pub mod text;
pub mod transport;

pub use config::{Binding, ConnectorsSettings, SlackSettings};
pub use decide::{Decision, Inbound, InboundFile, Lookup, decide};
pub use fold::{Outbound, TurnState};
pub use transport::{ChatTransport, Glyph};

use ginka_protocol::model::ConnectorState;

/// The Slack connector's id, as it appears in origins and settings.
pub const SLACK: &str = "slack";

/// What the daemon can ask of a hosted connector.
///
/// The connector runs inside the daemon but is not the service: the service
/// answers `ListConnectors` and `TestConnector` by asking whichever
/// connectors were registered with it, and knows nothing about Socket Mode.
pub trait ConnectorControl: Send + Sync + 'static {
    /// `slack`.
    fn id(&self) -> &'static str;
    /// How it is doing, for a client.
    fn state(&self) -> ConnectorState;
    /// The settings file changed under it.
    fn reload(&self, settings: &ConnectorsSettings);
    /// Post one message into a channel and take it back.
    fn test(&self, channel: &str) -> anyhow::Result<()>;
}

/// The state of a connector that is written down but not running: no
/// tokens, or a configuration with problems. What `ListConnectors` answers
/// for a connector nobody registered.
pub fn unconfigured_state(id: &str, settings: &ConnectorsSettings) -> ConnectorState {
    let slack = settings.slack.as_ref();
    ConnectorState {
        id: id.to_string(),
        enabled: slack.is_some_and(|slack| slack.enabled),
        connected: false,
        since: None,
        last_error: match slack {
            None => Some("not configured: no `connectors.slack` in settings.json".to_string()),
            Some(_) => Some("not running".to_string()),
        },
        bindings: slack
            .map(|slack| slack.bindings.iter().map(Binding::to_wire).collect())
            .unwrap_or_default(),
    }
}
