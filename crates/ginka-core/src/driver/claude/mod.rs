//! The Claude Code driver.
//!
//! Two halves that answer to different callers. The daemon's supervisor asks
//! for a command to run and hands lines back to be parsed; the process-level
//! session below launches the CLI itself and holds it open, which is what
//! makes steering possible (`docs/roadmap.md` §3.3 N1). Both read the vendor's
//! `stream-json` through the same `stream` module: one parser for one vendor.

mod control;
mod stream;

use anyhow::Result;
use ginka_protocol::AgentEvent;
use ginka_protocol::provider::{AccessMode, OptionOutcome, SessionOptions};
use serde_json::{Value, json};
use std::process::Command;
use std::sync::mpsc::Sender;

use super::{AgentDriver, CommandSpec, ParseState, ProviderModel, SessionSpec};
use crate::driver::{AgentProcess, AgentSession, DriverError, ProcessSessionSpec};

pub use stream::ClaudeStream;

/// The Claude Code CLI.
#[derive(Debug, Clone)]
pub struct ClaudeDriver {
    program: String,
    env: Vec<(String, String)>,
}

impl Default for ClaudeDriver {
    fn default() -> Self {
        Self::with_program("claude")
    }
}

impl ClaudeDriver {
    /// A driver that runs `program` instead of whatever is on `PATH`.
    ///
    /// Tests point this at the fake agent; a user with a version-managed
    /// install points it at their own binary.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            env: Vec::new(),
        }
    }

    /// Set an environment variable for every process this driver starts.
    ///
    /// A gateway's base URL, a version-managed install's `PATH`, or — in the
    /// tests — the script the fake agent should read.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The flags every invocation needs.
    ///
    /// `--verbose` is not decoration: without it the CLI collapses
    /// `stream-json` down to the final result, and the transcript would arrive
    /// as one block at the end of the turn.
    fn streaming_args(&self, spec: &SessionSpec) -> Vec<String> {
        let mut args = vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            // The other half of steering: with its input streamed too, the CLI
            // keeps reading while it works, so a message written mid-turn
            // reaches the turn rather than the next one (§3.3 N1). The prompt
            // arrives the same way, which is why nothing is passed positionally
            // below.
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--include-partial-messages".to_string(),
            "--verbose".to_string(),
        ];
        if let Some(model) = &spec.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        // The mode is the whole of what the agent may do without asking
        // (§3.3 N2) — passed always, because the vendor's own default is
        // not ours. What it would ask about is asked over the same pipes
        // (`control`) rather than refused, so the reader answers a card.
        args.push("--permission-mode".to_string());
        args.push(permission_mode(spec.access_mode).to_string());
        args.push("--permission-prompt-tool".to_string());
        args.push("stdio".to_string());
        if !spec.mcp_servers.is_empty() {
            // As a JSON string rather than a file: nothing is written into
            // the user's home for one session. The user's own servers still
            // load — this is not `--strict-mcp-config`.
            args.push("--mcp-config".to_string());
            args.push(crate::tools::claude_config(&spec.mcp_servers));
            // Headless, a tool the mode would ask about is refused rather
            // than asked about; the servers the daemon itself handed over
            // are ones it means the agent to use.
            args.push("--allowedTools".to_string());
            args.push(
                spec.mcp_servers
                    .iter()
                    .map(|server| format!("mcp__{}", server.name))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        args
    }
}

impl AgentDriver for ClaudeDriver {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn models(&self) -> Vec<ProviderModel> {
        // Aliases rather than dated ids: the CLI resolves them, so this list
        // does not go stale every time a model ships. The labels carry no
        // version for the same reason.
        [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
            .into_iter()
            .map(|(id, label)| ProviderModel::new(id, label))
            .collect()
    }

    fn apply_options(&self, _before: &SessionOptions, _after: &SessionOptions) -> OptionOutcome {
        // A resumed turn can carry a different model/effort while retaining
        // the provider's session id.
        OptionOutcome::Absorbed
    }

    fn program(&self) -> &str {
        &self.program
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec::new(&self.program).arg("--version")
    }

    /// `2.1.241 (Claude Code)` — the number is the first word.
    fn parse_version(&self, output: &str) -> Option<String> {
        let version = output.split_whitespace().next()?;
        (!version.is_empty()).then(|| version.to_string())
    }

    fn auth_command(&self) -> Option<CommandSpec> {
        Some(CommandSpec::new(&self.program).arg("auth").arg("status"))
    }

    /// The CLI answers with JSON: `{"loggedIn": false, "authMethod": …}`.
    fn parse_auth(&self, output: &str) -> Option<(bool, Option<String>)> {
        let value: Value = serde_json::from_str(output.trim()).ok()?;
        let signed_in = value.get("loggedIn")?.as_bool()?;
        let detail = value
            .get("authMethod")
            .and_then(Value::as_str)
            .filter(|method| *method != "none")
            .map(|method| format!("signed in with {method}"));
        Some((signed_in, detail))
    }

    /// Claude Code 2.x adds `email`, `orgName` and `subscriptionType` to the
    /// same JSON when signed in with claude.ai.
    fn parse_identity(&self, output: &str) -> Option<ginka_protocol::model::AccountIdentity> {
        let value: Value = serde_json::from_str(output.trim()).ok()?;
        if value.get("loggedIn").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let identity = ginka_protocol::model::AccountIdentity {
            email: text("email"),
            organization: text("orgName"),
            plan: text("subscriptionType"),
        };
        (identity != Default::default()).then_some(identity)
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program).args(self.streaming_args(spec));
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec {
        let mut command = CommandSpec::new(&self.program)
            .args(self.streaming_args(spec))
            .arg("--resume")
            .arg(vendor_session_id);
        for (key, value) in self.env.iter().chain(spec.env.iter()) {
            command = command.env(key, value);
        }
        command
    }

    fn supports_steer(&self) -> bool {
        true
    }

    /// Claude Code keeps its login, settings and sessions under one
    /// directory, `~/.claude` unless this says otherwise.
    fn home_variable(&self) -> Option<&'static str> {
        Some("CLAUDE_CONFIG_DIR")
    }

    /// The CLI has no `login` subcommand: starting it interactively in a
    /// directory with no login is what asks for one.
    fn login_command(&self) -> Option<CommandSpec> {
        Some(CommandSpec::new(&self.program))
    }

    fn encode_user_message(&self, text: &str) -> Option<String> {
        Some(PromptMessage::user(text).to_line())
    }

    fn supports_responses(&self) -> bool {
        true
    }

    /// `request_id` is the card [`control::request`] made.
    fn encode_response(&self, request_id: &str, response: &str) -> Option<String> {
        control::response(request_id, response)
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<AgentEvent> {
        // Asking before acting is the CLI talking to this client, not part of
        // the conversation `stream` reads.
        if line.contains("\"control_")
            && let Ok(message) = serde_json::from_str::<Value>(line.trim())
        {
            match message.get("type").and_then(Value::as_str) {
                Some("control_request") => {
                    state.recognized += 1;
                    let (events, reply) = control::request(&message);
                    state.outbox.extend(reply);
                    return events;
                }
                // A request withdrawn, or an answer to one of ours: the card
                // stays as history, and nothing needs saying.
                Some("control_cancel_request" | "control_response") => {
                    state.recognized += 1;
                    return Vec::new();
                }
                _ => {}
            }
        }
        // The reading itself lives in `stream`, so the fixtures that pin this
        // vendor's shapes have one implementation to pin.
        match state.stream.push_line(line) {
            Ok(events) => {
                if !events.is_empty() {
                    state.recognized += 1;
                }
                if let Some(id) = state.stream.session_id() {
                    state.vendor_session_id = Some(id.to_string());
                }
                // Each turn is its own process, so the reader's own count
                // restarts every time; the conversation's count is the one the
                // transcript and the checkpoints are filed against.
                let mut events = events;
                for event in &mut events {
                    match event {
                        AgentEvent::TurnEnd { turn } => {
                            state.turn += 1;
                            *turn = state.turn;
                        }
                        AgentEvent::TextDelta { .. } => state.streaming = true,
                        _ => {}
                    }
                }
                events
            }
            // A warning on stdout is not a message and not a failure.
            Err(DriverError::Malformed { .. }) => {
                state.unrecognized += 1;
                Vec::new()
            }
            // A shape we recognise in a form we do not: said out loud, because
            // a session that goes quiet with no explanation is worse.
            Err(error) => {
                state.unrecognized += 1;
                vec![AgentEvent::Unsupported {
                    shape: error.to_string(),
                }]
            }
        }
    }
}

impl ClaudeDriver {
    /// The driver id sessions and settings name Claude Code by.
    pub const ID: &'static str = "claude";

    /// The command a spec launches. Separate from starting it so the argument
    /// mapping — the part that breaks when a vendor renames a flag — can be
    /// read in a test without a process.
    pub fn command(spec: &ProcessSessionSpec) -> Command {
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

    /// Spawn `claude` for `spec` and start reading its stream into `events` on a
    /// background thread. Fails only when the process cannot be started.
    pub fn start(spec: &ProcessSessionSpec, events: Sender<AgentEvent>) -> Result<ClaudeSession> {
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
        // "Edit freely inside the worktree; commands are approved" — and
        // headless, a command that would be asked about is refused. The
        // vendor's `default` mode would refuse the edits too.
        AccessMode::Ask => "acceptEdits",
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

    /// True until the agent's output stream has closed and the process was reaped.
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
    /// A user message carrying `text` as the next prompt.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[&str]) -> (Vec<AgentEvent>, ParseState) {
        let driver = ClaudeDriver::default();
        let mut state = ParseState::default();
        let mut events = Vec::new();
        for line in lines {
            events.extend(driver.parse_line(line, &mut state));
        }
        (events, state)
    }

    #[test]
    fn the_start_command_asks_for_a_streamed_transcript() {
        let driver = ClaudeDriver::default();
        let command = driver.start_command(
            &SessionSpec::new("/tmp/wt", "write the test first")
                .with_model(Some("opus".to_string())),
        );
        assert_eq!(command.program, "claude");
        assert!(command.args.contains(&"--print".to_string()));
        assert_eq!(
            command
                .args
                .windows(2)
                .find(|pair| pair[0] == "--output-format"),
            Some(["--output-format".to_string(), "stream-json".to_string()].as_slice()),
        );
        // Without --verbose the CLI collapses the stream into one final blob.
        assert!(command.args.contains(&"--verbose".to_string()));
        assert!(command.args.contains(&"opus".to_string()));
        // Input is streamed too, which is what lets a message written while
        // the turn runs reach that turn (§3.3 N1).
        assert_eq!(
            command
                .args
                .windows(2)
                .find(|pair| pair[0] == "--input-format"),
            Some(["--input-format".to_string(), "stream-json".to_string()].as_slice()),
        );
        // And so the prompt is not on the command line at all: it is the first
        // message written to the agent.
        assert!(
            !command.args.contains(&"write the test first".to_string()),
            "{:?}",
            command.args
        );
        assert_eq!(
            driver
                .encode_user_message("write the test first")
                .as_deref()
                .map(|line| line.contains("write the test first")),
            Some(true)
        );
    }

    #[test]
    fn mcp_servers_ride_on_the_command_line_and_are_allowed() {
        let driver = ClaudeDriver::default();
        let spec =
            SessionSpec::new("/tmp/wt", "hello").with_mcp_servers(vec![crate::tools::McpServer {
                name: "ginka".into(),
                command: "/opt/ginka".into(),
                args: vec!["mcp".into()],
                env: vec![],
            }]);
        let command = driver.start_command(&spec);
        let config = command
            .args
            .windows(2)
            .find(|pair| pair[0] == "--mcp-config")
            .map(|pair| pair[1].clone())
            .expect("an --mcp-config");
        let parsed: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(parsed["mcpServers"]["ginka"]["command"], "/opt/ginka");
        assert_eq!(
            command
                .args
                .windows(2)
                .find(|pair| pair[0] == "--allowedTools"),
            Some(["--allowedTools".to_string(), "mcp__ginka".to_string()].as_slice())
        );
        // The user's own servers still load: this is not strict.
        assert!(!command.args.iter().any(|arg| arg == "--strict-mcp-config"));
        // And with none there is nothing on the command line about it.
        let bare = driver.start_command(&SessionSpec::new("/tmp/wt", "hello"));
        assert!(!bare.args.iter().any(|arg| arg == "--mcp-config"));
    }

    #[test]
    fn a_drivers_environment_reaches_the_process_it_starts() {
        let driver = ClaudeDriver::default().with_env("ANTHROPIC_BASE_URL", "http://localhost:1");
        let command = driver.start_command(&SessionSpec::new("/tmp/wt", "hello"));
        assert_eq!(
            command.env,
            vec![(
                "ANTHROPIC_BASE_URL".to_string(),
                "http://localhost:1".to_string()
            )]
        );
    }

    #[test]
    fn a_resume_continues_the_vendors_own_session() {
        let driver = ClaudeDriver::default();
        let command =
            driver.resume_command(&SessionSpec::new("/tmp/wt", "and now the fix"), "abc-123");
        assert_eq!(
            command.args.windows(2).find(|pair| pair[0] == "--resume"),
            Some(["--resume".to_string(), "abc-123".to_string()].as_slice()),
        );
    }

    #[test]
    fn the_version_is_read_out_of_what_the_cli_prints() {
        let driver = ClaudeDriver::default();
        assert_eq!(
            driver.parse_version("2.1.241 (Claude Code)\n").as_deref(),
            Some("2.1.241")
        );
        assert_eq!(driver.parse_version("  ").as_deref(), None);
    }

    #[test]
    fn being_signed_out_is_read_from_the_cli_rather_than_guessed() {
        // The user has to learn this before they send a prompt, not from a
        // session that failed.
        let driver = ClaudeDriver::default();
        assert_eq!(
            driver.parse_auth(r#"{"loggedIn": false, "authMethod": "none"}"#),
            Some((false, None))
        );
        assert_eq!(
            driver.parse_auth(r#"{"loggedIn": true, "authMethod": "oauth"}"#),
            Some((true, Some("signed in with oauth".into())))
        );
    }

    #[test]
    fn an_answer_this_driver_cannot_read_is_not_reported_as_signed_out() {
        // Refusing to start an agent that would have worked is the worse
        // failure of the two.
        let driver = ClaudeDriver::default();
        assert_eq!(driver.parse_auth("some new output"), None);
        assert_eq!(
            driver.parse_identity(
                r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "me@example.com", "orgName": "Acme", "subscriptionType": "max"}"#
            ),
            Some(ginka_protocol::model::AccountIdentity {
                email: Some("me@example.com".into()),
                organization: Some("Acme".into()),
                plan: Some("max".into()),
            })
        );
        assert_eq!(
            driver.parse_identity(r#"{"loggedIn": true, "authMethod": "oauth"}"#),
            None,
            "an older CLI that does not say is not guessed at"
        );
        assert_eq!(
            driver.parse_identity(r#"{"loggedIn": false, "email": "stale@example.com"}"#),
            None
        );
        assert_eq!(driver.parse_auth(""), None);
    }

    #[test]
    fn what_the_reader_learns_is_written_back_into_the_parse_state() {
        // The supervisor reads the session id, the turn count and whether the
        // driver understood anything from here, so the delegation has to feed
        // them rather than only returning events.
        let (events, state) = parse(&[
            r#"{"type":"system","subtype":"init","session_id":"abc-123","model":"m"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#,
        ]);
        assert_eq!(state.vendor_session_id.as_deref(), Some("abc-123"));
        assert!(state.streaming);
        assert_eq!(state.turn, 1);
        assert_eq!(state.recognized, 3);
        assert_eq!(state.unrecognized, 0);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text } if text == "hi"
        )));
    }

    #[test]
    fn a_line_that_is_not_a_message_is_counted_but_not_shown() {
        // Agents print warnings on stdout. One is not a reason to end a turn,
        // but a session that produced only these has hit a format change.
        let (events, state) = parse(&["Segmentation fault"]);
        assert!(events.is_empty());
        assert_eq!(state.unrecognized, 1);
        assert!(state.understood_nothing());
    }

    #[test]
    fn a_shape_this_build_does_not_know_is_surfaced_rather_than_dropped() {
        let (events, state) = parse(&[r#"{"type":"assistant","message":{}}"#]);
        assert_eq!(state.unrecognized, 1);
        assert!(matches!(
            events.first(),
            Some(AgentEvent::Unsupported { .. })
        ));
    }
}
