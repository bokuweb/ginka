//! The agent driver abstraction.
//!
//! A driver knows two things no other layer does: how to invoke one vendor's
//! CLI, and how to turn its output into [`AgentEvent`]s. Everything else about
//! running an agent — spawning, cancelling, persisting, pushing to clients —
//! is the same for every vendor and lives in [`crate::agent`].
//!
//! Both halves of a driver are pure: [`AgentDriver::start_command`] returns a
//! description of a command rather than running one, and
//! [`AgentDriver::parse_line`] is a function from a line to events. That is
//! what makes vendor formats testable against recorded fixtures instead of
//! against a live CLI (`docs/roadmap.md` §7 R6).

pub mod acp;
pub mod activity;
pub mod claude;
pub mod codex;
pub mod event;
pub mod probe;
pub mod process;
pub mod spec;
pub mod testing;
pub use activity::{ActivityItem, ActivityKind};
pub use claude::{ClaudeDriver, ClaudeSession, ClaudeStream};
pub use event::{AgentEvent, DriverError};

pub use probe::{ProbeResult, probe, probe_with_arg, resolve_binary, resolve_binary_in};
pub use process::AgentProcess;
/// What the `claude` driver's own command builder takes. The `SessionSpec`
/// below is the wider one the supervisor starts a session from.
pub use spec::SessionSpec as ProcessSessionSpec;

use anyhow::Result;
use ginka_protocol::model::PlanUsage;
pub use ginka_protocol::provider::ProviderModel;
use ginka_protocol::provider::{AccessMode, OptionOutcome, SessionOptions};
use std::path::PathBuf;
use std::sync::Arc;

/// A short-lived process that answers with an account's rate-limit windows
/// (`docs/accounts.md` §6).
///
/// Described rather than run, like every other command a driver produces.
/// `input` is written to the process line by line once it starts; the
/// driver's [`AgentDriver::parse_plan_usage`] reads the lines it prints
/// until one of them is the answer, and the process is stopped then rather
/// than waited for — a server that was told to read one thing does not exit
/// on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanUsageProbe {
    pub command: CommandSpec,
    pub input: Vec<String>,
}

/// A short-lived process that answers with the provider's current model list.
///
/// The request is described here because a catalogue is vendor-owned and can
/// change independently of Ginka. A driver's static [`AgentDriver::models`]
/// list is used whenever this probe is absent, times out, or cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalogueProbe {
    /// The long-running provider endpoint to start.
    pub command: CommandSpec,
    /// JSONL requests written to the endpoint in order.
    pub input: Vec<String>,
}

/// One provider-owned manual compaction process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSpec {
    /// The long-running provider endpoint to start.
    pub command: CommandSpec,
    /// Protocol messages written to the endpoint after it starts.
    pub input: Vec<String>,
}

/// A command to run, described rather than executed.
///
/// Returning this instead of a `std::process::Command` is what lets a test
/// assert which flags a driver chose without spawning anything.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Extra environment for the child, on top of the sanitized inherited one.
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    /// Build a spec for `program` with no arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// Append one argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Add an environment variable for the child.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// What a session is being started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    /// The worktree the agent runs in. Agents are scoped to one workspace.
    pub workspace_path: PathBuf,
    /// The opening prompt.
    pub prompt: String,
    /// What the agent is told before the prompt, and the transcript is not:
    /// the digest of a conversation moved here from another agent
    /// (`crate::handoff`). Sent once, on the turn that carries it.
    pub preamble: Option<String>,
    pub model: Option<String>,
    /// Provider reasoning level passed on every turn.
    pub reasoning_effort: Option<String>,
    /// Provider service tier passed on every turn.
    pub service_tier: Option<String>,
    /// What the agent may do without asking. A launch argument on every
    /// transport this build drives, so it is fixed for the process.
    pub access_mode: AccessMode,
    /// Extra environment, used by tests to point a driver at a fake agent.
    pub env: Vec<(String, String)>,
    /// The MCP servers the agent is told about (`crate::tools`). Each driver
    /// puts them on the command line in its vendor's own way.
    pub mcp_servers: Vec<crate::tools::McpServer>,
}

impl SessionSpec {
    /// A session in `workspace_path` opening with `prompt`.
    pub fn new(workspace_path: impl Into<PathBuf>, prompt: impl Into<String>) -> Self {
        Self {
            workspace_path: workspace_path.into(),
            prompt: prompt.into(),
            preamble: None,
            model: None,
            reasoning_effort: None,
            service_tier: None,
            access_mode: AccessMode::default(),
            env: Vec::new(),
            mcp_servers: Vec::new(),
        }
    }

    /// Tell the agent about these MCP servers.
    pub fn with_mcp_servers(mut self, servers: Vec<crate::tools::McpServer>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// Put `preamble` in front of the prompt, for the agent's eyes only.
    pub fn with_preamble(mut self, preamble: Option<String>) -> Self {
        self.preamble = preamble.filter(|text| !text.trim().is_empty());
        self
    }

    /// The message the agent is actually sent: the preamble, when there is
    /// one, then the prompt. The transcript records only the prompt, because
    /// the preamble is addressed to the agent and the reader has the original.
    pub fn agent_prompt(&self) -> String {
        match &self.preamble {
            Some(preamble) => format!("{preamble}\n\n---\n\n{}", self.prompt),
            None => self.prompt.clone(),
        }
    }

    /// Ask for a specific model.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// Ask for a provider reasoning level.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Ask for a provider service tier.
    pub fn with_service_tier(mut self, tier: Option<String>) -> Self {
        self.service_tier = tier;
        self
    }

    /// Fix what the agent may do without asking.
    pub fn with_access_mode(mut self, access_mode: AccessMode) -> Self {
        self.access_mode = access_mode;
        self
    }

    /// Add an environment variable for the agent process.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// What a driver carries between the lines of one session.
///
/// Vendors put the session id in a preamble and the accounting in a postscript,
/// and some of them stream a message twice — once as deltas and once whole. The
/// state is where that context lives, so `parse_line` stays a function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseState {
    /// The vendor's own id for this conversation, once it has told us. This is
    /// what a resume is built from.
    pub vendor_session_id: Option<String>,
    /// Set once the vendor has streamed a text delta, after which whole
    /// messages are ignored — otherwise every reply would be recorded twice.
    pub streaming: bool,
    /// Turn boundaries seen so far. Checkpoints are taken at these.
    pub turn: u32,
    /// Lines this driver understood.
    pub recognized: usize,
    /// Lines it did not. A session that ends with only these has hit a format
    /// change, and says so rather than reporting an empty success.
    pub unrecognized: usize,
    /// The `claude` reader's own state. Parsing is a fold over a session's
    /// lines — a tool result has to find the call it belongs to — so the state
    /// travels with the session rather than with the driver, which is shared.
    pub stream: claude::ClaudeStream,
    /// Lines the driver wants written to the agent's input, drained by the
    /// supervisor after every line it parses. How a transport that is a
    /// conversation — ACP answers `session/new` before it can be prompted —
    /// replies without the parser doing any I/O.
    pub outbox: Vec<String>,
    /// The ACP driver's half of the exchange.
    pub acp: acp::AcpState,
}

impl ParseState {
    /// Whether the driver failed to understand anything the agent said.
    ///
    /// This is the honest reading of a vendor that changed its output format:
    /// the process may well have exited zero, and reporting that as a finished
    /// session with an empty transcript is worse than reporting a failure.
    pub fn understood_nothing(&self) -> bool {
        self.recognized == 0 && self.unrecognized > 0
    }
}

/// One vendor's CLI, normalized.
pub trait AgentDriver: Send + Sync + 'static {
    /// The stable id used in the protocol and stored on sessions.
    fn id(&self) -> &'static str;

    /// What the agent picker shows.
    fn display_name(&self) -> &'static str;

    /// The models this driver offers. May be empty when the vendor decides.
    fn models(&self) -> Vec<ProviderModel>;

    /// How to ask the provider for its current model catalogue.
    fn model_catalogue_probe(&self) -> Option<ModelCatalogueProbe> {
        None
    }

    /// Read a complete catalogue from one line printed by the probe.
    fn parse_model_catalogue(&self, _line: &str) -> Option<Vec<ProviderModel>> {
        None
    }

    /// The binary this driver would run.
    fn program(&self) -> &str;

    /// A command that reports whether the CLI is installed, and its version.
    fn probe_command(&self) -> CommandSpec;

    /// Read a version out of what [`AgentDriver::probe_command`] printed.
    ///
    /// Vendors decorate it — `2.1.241 (Claude Code)`, `codex-cli 0.144.1` — so
    /// each driver knows where its own number is.
    fn parse_version(&self, output: &str) -> Option<String>;

    /// A command that reports whether the user is signed in, when the vendor
    /// offers one. `None` means this driver cannot tell, which is different
    /// from knowing the user is signed out.
    fn auth_command(&self) -> Option<CommandSpec> {
        None
    }

    /// Read the answer to [`AgentDriver::auth_command`].
    ///
    /// Returns `(signed in, what it said)`. A driver that cannot make sense of
    /// the output returns `None` rather than guessing: reporting a working
    /// agent as signed out would stop a user from starting it.
    fn parse_auth(&self, _output: &str) -> Option<(bool, Option<String>)> {
        None
    }

    /// The command that starts a fresh session.
    fn start_command(&self, spec: &SessionSpec) -> CommandSpec;

    /// What to write to the agent's input as soon as it starts, with
    /// whatever the driver needs to carry through the turn put in `state`.
    ///
    /// Empty for transports that take the whole turn on the command line. A
    /// driver that returns lines here is started with its input open, and
    /// the turn's end closes it.
    fn begin(
        &self,
        _spec: &SessionSpec,
        _vendor_session_id: Option<&str>,
        _state: &mut ParseState,
    ) -> Vec<String> {
        Vec::new()
    }

    /// The command that continues the vendor session `vendor_session_id`.
    ///
    /// Resume is what makes a transcript worth persisting: the daemon can be
    /// restarted and the conversation picked up rather than replayed.
    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec;

    /// Normalize one line of the agent's stdout.
    ///
    /// Returns every event the line carried, which may be none. Unrecognized
    /// lines are counted in `state` rather than raised: vendors print
    /// diagnostics on stdout, and one unknown line is not a failed session.
    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<AgentEvent>;

    /// Whether a user message can be written into a turn that is already
    /// running.
    ///
    /// A transport that says yes is handed the message on its standard input
    /// as it works; one that says no gets it as the next turn instead
    /// (`docs/roadmap.md` §3.3 N1). Saying yes also means the prompt itself
    /// arrives that way, so `start_command` must not put it on the command
    /// line.
    fn supports_steer(&self) -> bool {
        false
    }

    /// Decide whether later turns can use `after` without abandoning the
    /// provider's conversation. Drivers default to the conservative answer;
    /// a transport opts into carrying model, effort and tier on resume.
    fn apply_options(&self, _before: &SessionOptions, _after: &SessionOptions) -> OptionOutcome {
        OptionOutcome::RestartRequired
    }

    /// One user message, in whatever the transport reads from its input.
    /// `None` where there is no such thing.
    fn encode_user_message(&self, _text: &str) -> Option<String> {
        None
    }

    /// Build the provider-owned operation that compacts an idle conversation.
    ///
    /// `None` means manual compaction is not exposed by this transport. The
    /// daemon never guesses a slash command for an unsupported provider.
    fn compaction(&self, _spec: &SessionSpec, _vendor_session_id: &str) -> Option<CompactionSpec> {
        None
    }

    /// Whether the transport can answer a request without ending the turn.
    ///
    /// This is separate from steering: an agent may reject unsolicited input
    /// while still accepting a response to a question it opened.
    fn supports_responses(&self) -> bool {
        false
    }

    /// Encode one answer for the transport's live input stream.
    fn encode_response(&self, _request_id: &str, _response: &str) -> Option<String> {
        None
    }

    /// The environment variable this provider's CLI reads its state directory
    /// from, when it has one (`docs/accounts.md` §4).
    ///
    /// `None` means the CLI keeps one login per machine, and the provider
    /// cannot have a second account. Nothing above the driver spells the
    /// variable's name.
    fn home_variable(&self) -> Option<&'static str> {
        None
    }

    /// The vendor's own sign-in, to be run in a terminal with the account's
    /// environment. `None` for a CLI that has no such command.
    ///
    /// Ginka never performs a login itself: the browser round-trip and the
    /// token file are the vendor's, written into the account's directory.
    fn login_command(&self) -> Option<CommandSpec> {
        None
    }

    /// How to ask the provider for an account's rate-limit windows without
    /// running a turn. `None` for a provider that cannot be asked.
    fn plan_usage_probe(&self) -> Option<PlanUsageProbe> {
        None
    }

    /// Read the windows out of one line the probe printed, if this is the
    /// line that carries them.
    fn parse_plan_usage(&self, _line: &str) -> Option<PlanUsage> {
        None
    }
}

/// The drivers this build knows about.
///
/// Order is the order they are offered in; `claude` is first because it is the
/// one the roadmap makes load-bearing.
pub struct Registry {
    drivers: Vec<Arc<dyn AgentDriver>>,
}

impl Registry {
    /// An empty registry.
    pub fn empty() -> Self {
        Self {
            drivers: Vec::new(),
        }
    }

    /// Every driver this build ships, with their default binaries.
    pub fn with_defaults() -> Self {
        Self::from_settings(&Default::default())
    }

    /// Every driver this build ships, with the user's overrides applied.
    ///
    /// An override for an id this build has no driver for is ignored rather
    /// than refused: a settings file outlives the build that reads it, and a
    /// daemon that will not start because of a stale key is worse than one
    /// that starts without it.
    pub fn from_settings(settings: &crate::settings::DaemonSettings) -> Self {
        let mut registry = Self::empty();

        let claude = settings.agents.get("claude");
        let mut driver = claude::ClaudeDriver::with_program(
            claude
                .and_then(|agent| agent.program.clone())
                .unwrap_or_else(|| "claude".to_string()),
        );
        for (key, value) in claude.iter().flat_map(|agent| agent.env.iter()) {
            driver = driver.with_env(key, value);
        }
        registry.insert(Arc::new(driver));

        let codex = settings.agents.get("codex");
        let mut driver = codex::CodexDriver::with_program(
            codex
                .and_then(|agent| agent.program.clone())
                .unwrap_or_else(|| "codex".to_string()),
        );
        for (key, value) in codex.iter().flat_map(|agent| agent.env.iter()) {
            driver = driver.with_env(key, value);
        }
        registry.insert(Arc::new(driver));

        // Every agent reached over ACP, in the same shape.
        for base in [acp::AcpDriver::gemini(), acp::AcpDriver::opencode()] {
            let configured = settings.agents.get(base.id());
            let mut driver = match configured.and_then(|agent| agent.program.clone()) {
                Some(program) => base.with_program(program),
                None => base,
            };
            for (key, value) in configured.iter().flat_map(|agent| agent.env.iter()) {
                driver = driver.with_env(key, value);
            }
            registry.insert(Arc::new(driver));
        }

        registry
    }

    /// Add a driver, replacing any with the same id.
    ///
    /// Replacing rather than appending is what lets a test point the `claude`
    /// id at a fake agent binary without the real one shadowing it.
    pub fn insert(&mut self, driver: Arc<dyn AgentDriver>) {
        self.drivers.retain(|existing| existing.id() != driver.id());
        self.drivers.push(driver);
    }

    /// The driver with this id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn AgentDriver>> {
        self.drivers
            .iter()
            .find(|driver| driver.id() == id)
            .cloned()
    }

    /// Every id, in offering order.
    pub fn ids(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|driver| driver.id()).collect()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_registry_offers_claude_first() {
        let registry = Registry::with_defaults();
        assert_eq!(registry.ids().first(), Some(&"claude"));
        assert!(registry.get("claude").is_some());
        assert!(registry.get("codex").is_some());
        assert!(registry.get("nonesuch").is_none());
    }

    #[test]
    fn inserting_an_id_that_exists_replaces_it() {
        // A test points `claude` at a fake agent; the real driver must not
        // still be there to answer first.
        let mut registry = Registry::with_defaults();
        let before = registry.ids().len();
        registry.insert(Arc::new(claude::ClaudeDriver::with_program("/bin/echo")));
        assert_eq!(registry.ids().len(), before);
        assert_eq!(
            registry.get("claude").unwrap().probe_command().program,
            "/bin/echo"
        );
    }

    #[test]
    fn a_configured_binary_is_what_gets_started() {
        // A version-managed install, a wrapper script, or -- in the tests -- a
        // scripted stand-in.
        let mut settings = crate::settings::DaemonSettings::default();
        settings.agents.insert(
            "claude".to_string(),
            crate::settings::AgentSettings {
                program: Some("/opt/ginka/claude".to_string()),
                env: [(
                    "ANTHROPIC_BASE_URL".to_string(),
                    "http://gateway".to_string(),
                )]
                .into_iter()
                .collect(),
            },
        );
        let registry = Registry::from_settings(&settings);
        let command = registry
            .get("claude")
            .unwrap()
            .start_command(&SessionSpec::new("/tmp/wt", "hello"));
        assert_eq!(command.program, "/opt/ginka/claude");
        assert_eq!(
            command.env,
            vec![(
                "ANTHROPIC_BASE_URL".to_string(),
                "http://gateway".to_string()
            )]
        );
        assert_eq!(
            registry
                .get("codex")
                .unwrap()
                .start_command(&SessionSpec::new("/tmp/wt", "x"))
                .program,
            "codex",
            "an agent with no override keeps its default"
        );
    }

    #[test]
    fn an_override_for_an_agent_this_build_does_not_have_is_ignored() {
        // A settings file outlives the build that reads it.
        let mut settings = crate::settings::DaemonSettings::default();
        settings.agents.insert(
            "telepath".to_string(),
            crate::settings::AgentSettings::default(),
        );
        let registry = Registry::from_settings(&settings);
        assert_eq!(
            registry.ids(),
            vec!["claude", "codex", "gemini", "opencode"]
        );
    }

    #[test]
    fn a_state_that_understood_nothing_is_distinguishable_from_a_quiet_session() {
        let quiet = ParseState::default();
        assert!(
            !quiet.understood_nothing(),
            "no output is not a format change"
        );
        let changed = ParseState {
            unrecognized: 12,
            ..ParseState::default()
        };
        assert!(changed.understood_nothing());
    }
}

// ---------------------------------------------------------------------------
// Session policy: what happens to a message typed while a turn is running, and
// what a change of options costs. Shared by every driver, so it lives beside
// the trait rather than inside one (`docs/roadmap.md` §3.3 N1, N2).

/// Where a session is in its lifecycle, as far as dispatch is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// The process is starting or the handshake is not finished.
    Connecting,
    /// Connected with no turn running.
    Idle,
    /// A turn is in flight.
    Turn,
}

/// What should happen to a message the user just submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// Empty input; nothing to do.
    Nothing,
    /// Open a new turn with this text.
    StartTurn(String),
    /// Inject into the turn already running.
    Steer(String),
    /// Held until the current turn settles; visible above the composer.
    Queued,
}

/// The composer's dispatch policy for one session.
///
/// Holding the queue here rather than in a view keeps "what happens to this
/// message" testable, and keeps the answer identical in the UI, the CLI and
/// anything else that drives a session.
#[derive(Debug, Default, Clone)]
pub struct FollowUps {
    pending: Vec<String>,
}

impl FollowUps {
    /// Decide what to do with a submitted message.
    ///
    /// `supports_steer` is the *transport's* answer, asked of the live
    /// session: the same provider can support it on one release and not the
    /// next, so it is a parameter rather than a property of the provider.
    pub fn submit(&mut self, message: &str, phase: SessionPhase, supports_steer: bool) -> Dispatch {
        let message = message.trim();
        if message.is_empty() {
            return Dispatch::Nothing;
        }
        match phase {
            SessionPhase::Idle => Dispatch::StartTurn(message.to_string()),
            SessionPhase::Turn if supports_steer => Dispatch::Steer(message.to_string()),
            // Still connecting, or a transport with no way in: hold it where
            // the user can still see it.
            SessionPhase::Connecting | SessionPhase::Turn => {
                self.pending.push(message.to_string());
                Dispatch::Queued
            }
        }
    }

    /// The transport took the steered message; there is nothing left to hold.
    pub fn steer_accepted(&mut self) {}

    /// The transport refused the steered message. It goes to the front of the
    /// queue — ahead of anything typed after it — rather than being lost.
    pub fn steer_rejected(&mut self, message: &str) {
        let message = message.trim();
        if !message.is_empty() {
            self.pending.insert(0, message.to_string());
        }
    }

    /// Called when a turn settles. Everything held opens one turn together:
    /// two follow-ups typed during one turn are one thought, and splitting
    /// them into two turns makes the agent answer the first without the
    /// second.
    pub fn turn_finished(&mut self) -> Option<Dispatch> {
        if self.pending.is_empty() {
            return None;
        }
        let joined = std::mem::take(&mut self.pending).join("\n\n");
        Some(Dispatch::StartTurn(joined))
    }

    /// What the composer shows as still waiting.
    pub fn pending(&self) -> &[String] {
        &self.pending
    }
}

/// One running agent session.
pub trait AgentSession {
    /// Whether a message can be injected into the turn already running.
    fn supports_steer(&self) -> bool;

    /// Inject a message into the running turn. The outcome is asynchronous:
    /// the transport answers with a steer-accepted or steer-rejected event.
    fn steer(&mut self, message: &str) -> Result<()>;

    /// Try to apply new options without restarting, answering whether the
    /// transport managed it.
    fn apply_options(&mut self, options: &SessionOptions) -> Result<OptionOutcome>;

    /// Stop the current turn. Whether the process survives is the driver's
    /// business, not the caller's.
    fn cancel(&mut self) -> Result<()>;
}

/// Apply an option change to a running session, updating `current` when the
/// session took it.
///
/// The access mode never reaches the driver: loosening or tightening what an
/// already-running agent may touch deserves a fresh session even where the
/// transport would accept the change on its next turn.
pub fn apply_session_options(
    session: &mut dyn AgentSession,
    current: &mut SessionOptions,
    next: SessionOptions,
) -> Result<OptionOutcome> {
    if !next.differs_from(current) {
        return Ok(OptionOutcome::Absorbed);
    }
    if SessionOptions::forces_restart(current, &next) {
        return Ok(OptionOutcome::RestartRequired);
    }
    let outcome = session.apply_options(&next)?;
    if outcome.absorbed() {
        *current = next;
    }
    Ok(outcome)
}
