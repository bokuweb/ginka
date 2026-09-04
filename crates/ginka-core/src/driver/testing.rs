//! A session that answers however a test needs it to.
//!
//! Agent behaviour is pinned against this rather than a live vendor CLI: no
//! tokens, no network, and the awkward answers — a refused steer, a transport
//! that cannot absorb a model change — are the ones that are hard to provoke
//! for real and easy to script here.

use anyhow::{Result, bail};
use ginka_protocol::provider::{OptionOutcome, SessionOptions};

use super::AgentSession;

#[derive(Debug, Default, Clone)]
pub struct ScriptedSession {
    supports_steer: bool,
    absorbs_options: bool,
    options_fail: bool,
    steered: Vec<String>,
    applied: Vec<SessionOptions>,
    cancels: usize,
}

impl ScriptedSession {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn steering(mut self, supported: bool) -> Self {
        self.supports_steer = supported;
        self
    }

    #[must_use]
    pub fn absorbs_options(mut self, absorbs: bool) -> Self {
        self.absorbs_options = absorbs;
        self
    }

    /// The transport errors out when asked to change options.
    #[must_use]
    pub fn failing_options(mut self) -> Self {
        self.options_fail = true;
        self
    }

    pub fn steered(&self) -> &[String] {
        &self.steered
    }

    pub fn applied(&self) -> Vec<SessionOptions> {
        self.applied.clone()
    }

    pub fn cancels(&self) -> usize {
        self.cancels
    }
}

impl AgentSession for ScriptedSession {
    fn supports_steer(&self) -> bool {
        self.supports_steer
    }

    fn steer(&mut self, message: &str) -> Result<()> {
        if !self.supports_steer {
            bail!("this transport cannot steer a running turn");
        }
        self.steered.push(message.to_string());
        Ok(())
    }

    fn apply_options(&mut self, options: &SessionOptions) -> Result<OptionOutcome> {
        if self.options_fail {
            bail!("the transport failed to apply the options");
        }
        self.applied.push(options.clone());
        Ok(if self.absorbs_options {
            OptionOutcome::Absorbed
        } else {
            OptionOutcome::RestartRequired
        })
    }

    fn cancel(&mut self) -> Result<()> {
        self.cancels += 1;
        Ok(())
    }
}
