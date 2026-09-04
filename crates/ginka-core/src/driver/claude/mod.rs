//! The Claude Code driver.
//!
//! Launches the CLI in streaming mode — output *and* input — and holds the
//! session it produces. Streaming input is not a detail: it is what makes
//! steering possible at all (`docs/roadmap.md` §3.3 N1). Without it a
//! follow-up can only wait for the turn to end.

mod stream;

use anyhow::Result;
use ginka_protocol::provider::{AccessMode, OptionOutcome, SessionOptions};
use serde_json::json;
use std::process::Command;
use std::sync::mpsc::Sender;

use crate::driver::{AgentEvent, AgentProcess, AgentSession, SessionSpec};

pub use stream::ClaudeStream;

/// Starts Claude Code sessions.
pub struct ClaudeDriver;

impl ClaudeDriver {
    pub const ID: &'static str = "claude";

    /// The command a spec launches. Separate from starting it so the argument
    /// mapping — the part that breaks when a vendor renames a flag — can be
    /// read in a test without a process.
    pub fn command(spec: &SessionSpec) -> Command {
        let mut command = Command::new(&spec.binary);
        command
            .current_dir(&spec.cwd)
            // Print, do not open a UI.
            .arg("--print")
            .args(["--output-format", "stream-json"])
            .args(["--input-format", "stream-json"])
            // stream-json output is only complete with this on.
            .arg("--verbose")
            .args([
                "--permission-mode",
                permission_mode(spec.options.access_mode),
            ]);

        if let Some(model) = &spec.options.model {
            command.args(["--model", model]);
        }
        if let Some(session) = &spec.resume {
            command.args(["--resume", session]);
        }
        command
    }

    pub fn start(spec: &SessionSpec, events: Sender<AgentEvent>) -> Result<ClaudeSession> {
        let mut session = Self::start_with(Self::command(spec), events)?;
        session.options = spec.options.clone();
        Ok(session)
    }

    /// Start from a prepared command. The seam tests use to run a session
    /// against the fake agent instead of a vendor CLI.
    pub fn start_with(command: Command, events: Sender<AgentEvent>) -> Result<ClaudeSession> {
        Ok(ClaudeSession {
            process: AgentProcess::spawn(command, events)?,
            options: SessionOptions::default(),
        })
    }
}

/// Our access modes in the vendor's vocabulary. Mapped once, here, so no view
/// and no other driver has to know these names.
fn permission_mode(access: AccessMode) -> &'static str {
    match access {
        // Plan mode reads and proposes without touching anything.
        AccessMode::ReadOnly => "plan",
        AccessMode::Ask => "default",
        AccessMode::Auto => "bypassPermissions",
    }
}

/// A live Claude Code session.
#[derive(Debug)]
pub struct ClaudeSession {
    process: AgentProcess,
    options: SessionOptions,
}

impl ClaudeSession {
    /// Start a turn.
    pub fn prompt(&mut self, text: &str) -> Result<()> {
        self.send_raw(&PromptMessage::user(text).to_line())
    }

    /// Write a line to the agent verbatim.
    pub fn send_raw(&mut self, line: &str) -> Result<()> {
        self.process.send_line(line)
    }

    /// Close the agent's input. On this transport end-of-file is how a session
    /// is ended without a signal.
    pub fn close_input(&mut self) -> Result<()> {
        self.process.close_input()
    }

    /// The provider's session id, once it has introduced itself — what the
    /// next resume is built from.
    pub fn session_id(&self) -> Option<String> {
        self.process.session_id()
    }

    pub fn is_running(&self) -> bool {
        self.process.is_running()
    }
}

impl AgentSession for ClaudeSession {
    /// Streaming input means a user message can be written while a turn is
    /// still running, which is exactly what steering is.
    fn supports_steer(&self) -> bool {
        true
    }

    fn steer(&mut self, message: &str) -> Result<()> {
        // The same shape as a prompt: on this transport a steered message *is*
        // a user message, delivered mid-turn instead of between turns.
        self.send_raw(&PromptMessage::user(message).to_line())
    }

    fn apply_options(&mut self, options: &SessionOptions) -> Result<OptionOutcome> {
        // The permission mode is a launch argument: there is no way to widen
        // or narrow it without a new process.
        if options.access_mode != self.options.access_mode {
            return Ok(OptionOutcome::RestartRequired);
        }
        // Model, effort and tier ride on the next turn, so the session keeps
        // its context and its process.
        self.options = options.clone();
        Ok(OptionOutcome::Absorbed)
    }

    fn cancel(&mut self) -> Result<()> {
        self.process.cancel()
    }
}

/// One message written to the agent's stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    text: String,
}

impl PromptMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// The wire form: one JSON object on one line, whatever the text contains.
    pub fn to_line(&self) -> String {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": self.text}],
            }
        })
        .to_string()
    }
}
