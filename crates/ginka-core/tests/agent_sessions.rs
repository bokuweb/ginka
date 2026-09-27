//! Running an agent, end to end, against the fake agent binary.
//!
//! Nothing here talks to a vendor: the scripted stand-in speaks Claude Code's
//! `stream-json`, so the real driver parses it and only the process on the far
//! end is fake. That is what makes supervision, cancellation, queued follow-ups
//! and resume testable without a network or a token budget.

mod support;

use ginka_core::driver::{
    AgentDriver, CommandSpec, CompactionSpec, ParseState, Registry, SessionSpec,
    claude::ClaudeDriver, codex::CodexDriver,
};
use ginka_core::service::{EventSink, Service};
use ginka_core::{Paths, db};
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::model::{SessionState, TranscriptEntry, TranscriptPayload};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{AgentEvent, ProjectName, SessionId, WorkspaceId};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The scripted agent this test run built.
const FAKE_AGENT: &str = env!("CARGO_BIN_EXE_ginka-fake-agent");

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<DaemonEvent>>,
}

impl Recorder {
    fn all(&self) -> Vec<DaemonEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl EventSink for Recorder {
    fn emit(&self, event: DaemonEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// Codex's test transport with the opposite N2 answer, so the replacement
/// path is exercised without a live provider.
struct RestartDriver {
    inner: CodexDriver,
}

/// A scripted transport that can pause on a normalized question and accepts
/// the answer on the same input stream. The shipped CLI transports do not
/// expose this yet; this pins the daemon contract without a live vendor.
struct ResponseDriver {
    inner: ClaudeDriver,
}

impl ResponseDriver {
    fn new(program: &str, script: &std::path::Path) -> Self {
        Self {
            inner: ClaudeDriver::with_program(program)
                .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
        }
    }
}

impl AgentDriver for ResponseDriver {
    fn id(&self) -> &'static str {
        "response-agent"
    }

    fn display_name(&self) -> &'static str {
        "Response agent"
    }

    fn models(&self) -> Vec<ginka_protocol::ProviderModel> {
        self.inner.models()
    }

    fn compaction(&self, spec: &SessionSpec, vendor_session_id: &str) -> Option<CompactionSpec> {
        let mut compact = spec.clone();
        compact.prompt = "/compact".into();
        Some(CompactionSpec {
            command: self.inner.resume_command(&compact, vendor_session_id),
            input: Vec::new(),
        })
    }

    fn program(&self) -> &str {
        self.inner.program()
    }

    fn probe_command(&self) -> CommandSpec {
        self.inner.probe_command()
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        self.inner.parse_version(output)
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        self.inner.start_command(spec)
    }

    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec {
        self.inner.resume_command(spec, vendor_session_id)
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<AgentEvent> {
        if let Some(question) = line.strip_prefix("GINKA_ASK ") {
            return vec![AgentEvent::AskUser {
                id: "ask-1".into(),
                question: question.into(),
                options: vec!["SQLite".into(), "Postgres".into()],
            }];
        }
        self.inner.parse_line(line, state)
    }

    fn supports_steer(&self) -> bool {
        self.inner.supports_steer()
    }

    fn encode_user_message(&self, text: &str) -> Option<String> {
        self.inner.encode_user_message(text)
    }

    fn supports_responses(&self) -> bool {
        true
    }

    fn encode_response(&self, _request_id: &str, response: &str) -> Option<String> {
        self.inner.encode_user_message(response)
    }
}

impl RestartDriver {
    fn new(program: &str, script: &std::path::Path) -> Self {
        Self {
            inner: CodexDriver::with_program(program)
                .with_exec()
                .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
        }
    }
}

impl AgentDriver for RestartDriver {
    fn id(&self) -> &'static str {
        "restart-codex"
    }

    fn display_name(&self) -> &'static str {
        "Restart Codex"
    }

    fn models(&self) -> Vec<ginka_protocol::ProviderModel> {
        self.inner.models()
    }

    fn program(&self) -> &str {
        self.inner.program()
    }

    fn probe_command(&self) -> CommandSpec {
        self.inner.probe_command()
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        self.inner.parse_version(output)
    }

    fn start_command(&self, spec: &SessionSpec) -> CommandSpec {
        self.inner.start_command(spec)
    }

    fn resume_command(&self, spec: &SessionSpec, vendor_session_id: &str) -> CommandSpec {
        self.inner.resume_command(spec, vendor_session_id)
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Vec<ginka_protocol::AgentEvent> {
        self.inner.parse_line(line, state)
    }

    fn apply_options(
        &self,
        _before: &ginka_protocol::SessionOptions,
        _after: &ginka_protocol::SessionOptions,
    ) -> ginka_protocol::OptionOutcome {
        ginka_protocol::OptionOutcome::RestartRequired
    }
}

struct Fixture {
    service: Service,
    /// What a restart needs to build the same daemon again: its state
    /// directory, its database file and its drivers.
    paths: Paths,
    db_path: std::path::PathBuf,
    recorder: Arc<Recorder>,
    workspace: WorkspaceId,
    script: std::path::PathBuf,
    _home: tempfile::TempDir,
    /// Kept alive: dropping it removes the repository under test.
    _work: tempfile::TempDir,
}

impl Fixture {
    /// A registered project with one workspace, and a `claude` driver that
    /// actually runs the fake agent.
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(home.path().join("state"));
        paths.ensure().unwrap();

        // One script file per fixture, rewritten before each turn: the agent
        // reads it when it starts, so a follow-up re-runs whatever is there.
        let script = work.path().join("agent-script.txt");
        let recorder = Arc::new(Recorder::default());
        let drivers = Self::drivers(&script);
        let db_path = home.path().join("fixture.db");
        let mut service =
            Service::new(paths.clone(), db::open(&db_path).unwrap(), recorder.clone())
                .with_drivers(drivers);

        let root = work.path().join("comet");
        support::repository(&root);
        service
            .handle(Request::AddProject {
                path: root,
                label: None,
            })
            .unwrap();
        let workspace = match service
            .handle(Request::CreateWorkspace {
                project: ProjectName("comet".into()),
                branch: "harbor".into(),
                base: None,
            })
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id(),
            other => panic!("expected a workspace, got {other:?}"),
        };

        Self {
            service,
            paths,
            db_path,
            recorder,
            workspace,
            script,
            _home: home,
            _work: work,
        }
    }

    /// Stop this daemon and start another over the same state, the way a
    /// restart does: nothing held in memory survives, only what was stored.
    fn restart(&mut self) {
        let service = Service::new(
            self.paths.clone(),
            db::open(&self.db_path).unwrap(),
            self.recorder.clone(),
        )
        .with_drivers(Self::drivers(&self.script));
        self.service = service;
    }

    /// Run `codex` over the fake app server instead of scripted `exec`.
    fn use_codex_app_server(&mut self) {
        let mut drivers = Self::drivers(&self.script);
        drivers.insert(Arc::new(CodexDriver::with_program(FAKE_AGENT)));
        self.service = Service::new(
            self.paths.clone(),
            db::open(&self.db_path).unwrap(),
            self.recorder.clone(),
        )
        .with_drivers(drivers);
    }

    /// The drivers every fixture daemon runs: the fake agent behind each.
    fn drivers(script: &std::path::Path) -> Registry {
        let mut drivers = Registry::with_defaults();
        drivers.insert(Arc::new(
            ClaudeDriver::with_program(FAKE_AGENT)
                .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
        ));
        // The same fake agent behind a driver that cannot steer, so the
        // fallback — queue the follow-up, resume afterwards — stays covered.
        drivers.insert(Arc::new(
            ginka_core::driver::codex::CodexDriver::with_program(FAKE_AGENT)
                .with_exec()
                .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
        ));
        drivers.insert(Arc::new(RestartDriver::new(FAKE_AGENT, script)));
        drivers.insert(Arc::new(ResponseDriver::new(FAKE_AGENT, script)));
        drivers.insert(Arc::new(
            ginka_core::driver::acp::AcpDriver::gemini().with_program(FAKE_AGENT),
        ));
        drivers
    }

    /// Point the agent at `script`, and start a session with `prompt`.
    fn start(&mut self, script: &str, prompt: &str) -> SessionId {
        self.start_with("claude", script, prompt)
    }

    /// The same, against a named driver.
    fn start_with(&mut self, agent: &str, script: &str, prompt: &str) -> SessionId {
        std::fs::write(&self.script, script).unwrap();
        match self
            .service
            .handle(Request::StartSession {
                workspace: self.workspace.clone(),
                agent: agent.into(),
                prompt: prompt.into(),
                model: None,
                reasoning_effort: None,
                service_tier: None,
                account: None,
                access_mode: None,
                origin: None,
            })
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("expected a session, got {other:?}"),
        }
    }

    fn state(&mut self, id: &SessionId) -> SessionState {
        match self
            .service
            .handle(Request::ListSessions {
                workspace: None,
                origin: None,
            })
            .unwrap()
        {
            Response::Sessions { sessions } => {
                sessions
                    .into_iter()
                    .find(|session| &session.id == id)
                    .expect("the session is stored")
                    .state
            }
            other => panic!("expected sessions, got {other:?}"),
        }
    }

    fn wait_for_state(&mut self, id: &SessionId, wanted: SessionState) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = self.state(id);
            if state == wanted {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the session never reached {wanted:?}; last state was {state:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Wait for a session to reach a state it will not leave.
    fn settle(&mut self, id: &SessionId) -> SessionState {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = self.state(id);
            if !matches!(
                state,
                SessionState::Starting | SessionState::Running | SessionState::AwaitingInput
            ) {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "the session never left {state:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A path in the test's own scratch directory.
    fn path(&self, name: &str) -> std::path::PathBuf {
        self._work.path().join(name)
    }

    /// Where the workspace's worktree is on disk.
    fn workspace_path(&mut self) -> std::path::PathBuf {
        match self
            .service
            .handle(Request::ListWorkspaces { project: None })
            .unwrap()
        {
            Response::Workspaces { workspaces } => {
                workspaces
                    .into_iter()
                    .find(|summary| summary.id() == self.workspace)
                    .expect("the workspace is listed")
                    .worktree
                    .path
            }
            other => panic!("expected workspaces, got {other:?}"),
        }
    }

    fn checkpoints(&mut self) -> Vec<ginka_protocol::model::Checkpoint> {
        match self
            .service
            .handle(Request::ListCheckpoints {
                workspace: self.workspace.clone(),
            })
            .unwrap()
        {
            Response::Checkpoints { checkpoints } => checkpoints,
            other => panic!("expected checkpoints, got {other:?}"),
        }
    }

    /// Wait until the transcript holds the turn boundaries `done` accepts.
    fn wait_for(&mut self, session: &SessionId, done: impl Fn(&[u32]) -> bool) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let turns: Vec<u32> = self
                .transcript(session)
                .iter()
                .filter_map(|entry| match entry {
                    TranscriptPayload::Agent {
                        event: AgentEvent::TurnEnd { turn },
                    } => Some(*turn),
                    _ => None,
                })
                .collect();
            if done(&turns) {
                return turns;
            }
            assert!(
                Instant::now() < deadline,
                "the turns never arrived: {turns:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Ask the service, and take the answer.
    fn ask(&mut self, request: Request) -> Response {
        self.service
            .handle(request)
            .unwrap_or_else(|error| panic!("request failed: {error}"))
    }

    /// The transcript once the agent has said `needle`, or a panic after a
    /// while: a turn started by a request lands asynchronously, so its state
    /// alone cannot say whether it has begun.
    fn wait_for_said(&mut self, id: &SessionId, needle: &str) -> Vec<TranscriptPayload> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let transcript = self.transcript(id);
            if spoken(&transcript).contains(needle) {
                self.settle(id);
                return self.transcript(id);
            }
            assert!(
                Instant::now() < deadline,
                "the agent never said {needle:?}: {transcript:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The stored session, as the daemon has it.
    fn stored(&mut self, id: &SessionId) -> ginka_protocol::model::Session {
        match self
            .service
            .handle(Request::ListSessions {
                workspace: None,
                origin: None,
            })
            .unwrap()
        {
            Response::Sessions { sessions } => sessions
                .into_iter()
                .find(|session| &session.id == id)
                .expect("the session is stored"),
            other => panic!("expected sessions, got {other:?}"),
        }
    }

    fn transcript(&mut self, id: &SessionId) -> Vec<TranscriptPayload> {
        match self
            .service
            .handle(Request::SessionTranscript {
                session: id.clone(),
                after: None,
                limit: None,
            })
            .unwrap()
        {
            Response::Transcript { entries } => {
                entries.into_iter().map(|entry| entry.payload).collect()
            }
            other => panic!("expected a transcript, got {other:?}"),
        }
    }

    /// The stored transcript, positions and all.
    fn transcript_entries(
        &mut self,
        id: &SessionId,
    ) -> Vec<ginka_protocol::model::TranscriptEntry> {
        match self.ask(Request::SessionTranscript {
            session: id.clone(),
            after: None,
            limit: None,
        }) {
            Response::Transcript { entries } => entries,
            other => panic!("expected a transcript, got {other:?}"),
        }
    }
}

/// The text an agent produced, concatenated.
fn spoken(transcript: &[TranscriptPayload]) -> String {
    transcript
        .iter()
        .filter_map(|entry| match entry {
            TranscriptPayload::Agent {
                event: AgentEvent::TextDelta { text },
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_session_records_the_prompt_and_the_agents_answer() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"on it"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"on it","session_id":"vendor-1","usage":{"input_tokens":3,"output_tokens":2}}"#,
        ]
        .join("\n"),
        "write the test first",
    );

    assert_eq!(fixture.settle(&session), SessionState::Finished);
    let transcript = fixture.transcript(&session);
    assert_eq!(
        transcript.first(),
        Some(&TranscriptPayload::User {
            text: "write the test first".into()
        }),
        "the prompt opens the transcript"
    );
    assert_eq!(spoken(&transcript), "on it");
    assert!(
        transcript.iter().any(|entry| matches!(
            entry,
            TranscriptPayload::Agent {
                event: AgentEvent::TurnStarted {
                    provider: Some(provider),
                    model: None,
                    reasoning_effort: None,
                    service_tier: None,
                }
            } if provider == "claude"
        )),
        "the supervisor records effective turn provenance before vendor output: {transcript:?}"
    );
    assert!(
        transcript.iter().any(|entry| matches!(
            entry,
            TranscriptPayload::Agent {
                event: AgentEvent::TurnEnd { turn: 1 }
            }
        )),
        "the turn boundary is recorded: {transcript:?}"
    );
}

#[test]
fn manual_compaction_is_an_idle_resumed_turn_owned_by_the_provider() {
    let mut fixture = Fixture::new();
    let session = fixture.start_with(
        "response-agent",
        &[
            r#"{"type":"system","subtype":"init","session_id":"compact-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"compact-1"}"#,
        ]
        .join("\n"),
        "inspect the repository",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    assert_eq!(
        fixture.ask(Request::CompactSession {
            session: session.clone(),
        }),
        Response::Ack
    );
    fixture.wait_for(&session, |turns| turns.len() == 2);

    assert!(
        !fixture
            .transcript(&session)
            .iter()
            .any(|entry| matches!(entry, TranscriptPayload::User { text } if text == "/compact")),
        "provider controls are not ordinary user prompts"
    );
}

#[test]
fn manual_compaction_refuses_an_active_turn_instead_of_queuing() {
    let mut fixture = Fixture::new();
    let session = fixture.start_with(
        "response-agent",
        &[
            r#"{"type":"system","subtype":"init","session_id":"compact-busy"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#,
            "#sleep 60000",
        ]
        .join("\n"),
        "keep working",
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while spoken(&fixture.transcript(&session)).is_empty() {
        assert!(Instant::now() < deadline, "the agent never started");
        std::thread::sleep(Duration::from_millis(20));
    }

    let error = fixture
        .service
        .handle(Request::CompactSession {
            session: session.clone(),
        })
        .expect_err("compaction must not be queued behind a running turn");
    assert_eq!(error.code, "failed");
    assert!(error.message.contains("still working"));
    fixture
        .service
        .handle(Request::CancelSession { session })
        .unwrap();
}

#[test]
fn manual_compaction_refuses_a_provider_without_an_explicit_operation() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"claude-compact"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"claude-compact"}"#,
        ]
        .join("\n"),
        "inspect the repository",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    let error = fixture
        .service
        .handle(Request::CompactSession { session })
        .expect_err("unsupported providers must not receive a guessed command");
    assert_eq!(error.code, "failed");
    assert!(error.message.contains("does not support"));
}

#[test]
fn a_response_is_delivered_only_to_the_request_the_agent_is_waiting_on() {
    let mut fixture = Fixture::new();
    let session = fixture.start_with(
        "response-agent",
        &[
            r#"{"type":"system","subtype":"init","session_id":"interactive-1"}"#,
            "GINKA_ASK Which database?",
            "#read",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"using {stdin}"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"interactive-1"}"#,
        ]
        .join("\n"),
        "choose the storage",
    );
    fixture.wait_for_state(&session, SessionState::AwaitingInput);

    let wrong = fixture
        .service
        .handle(Request::RespondToAgent {
            session: session.clone(),
            request_id: "some-other-request".into(),
            response: "Redis".into(),
        })
        .expect_err("a stale card must not become an ordinary follow-up");
    assert_eq!(wrong.code, "failed");
    assert_eq!(fixture.state(&session), SessionState::AwaitingInput);

    assert_eq!(
        fixture.ask(Request::RespondToAgent {
            session: session.clone(),
            request_id: "ask-1".into(),
            response: "SQLite".into(),
        }),
        Response::Ack
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    let transcript = fixture.transcript(&session);
    assert!(transcript.iter().any(|entry| matches!(
        entry,
        TranscriptPayload::Response {
            request_id,
            text,
        } if request_id == "ask-1" && text == "SQLite"
    )));
    assert_eq!(spoken(&transcript), "using SQLite");
}

#[test]
fn the_vendors_session_id_is_kept_so_the_conversation_can_be_continued() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-42"}"#,
        "hello",
    );
    fixture.settle(&session);

    match fixture
        .service
        .handle(Request::ListSessions {
            workspace: None,
            origin: None,
        })
        .unwrap()
    {
        Response::Sessions { sessions } => assert_eq!(
            sessions[0].vendor_session_id.as_deref(),
            Some("vendor-42"),
            "without this the session could be replayed but not resumed"
        ),
        other => panic!("expected sessions, got {other:?}"),
    }
}

#[test]
fn events_reach_clients_while_the_agent_is_still_working() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        ]
        .join("\n"),
        "hello",
    );
    fixture.settle(&session);

    let events = fixture.recorder.all();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, DaemonEvent::SessionStarted { .. })),
        "a new session is announced: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            DaemonEvent::SessionEvent {
                entry: TranscriptEntry {
                    payload: TranscriptPayload::Agent {
                        event: AgentEvent::TextDelta { .. }
                    },
                    ..
                },
                ..
            }
        )),
        "text is pushed as it arrives: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            DaemonEvent::SessionEvent {
                entry: TranscriptEntry {
                    payload: TranscriptPayload::User { .. },
                    ..
                },
                ..
            }
        )),
        "the user's own prompt is pushed too, or a window has to wait for a \
         poll to show what was just typed: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            DaemonEvent::SessionStateChanged {
                state: SessionState::Running,
                ..
            }
        )),
        "a window has to be able to show that the agent started: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            DaemonEvent::SessionStateChanged {
                state: SessionState::Finished,
                ..
            }
        )),
        "the end of the session is announced: {events:?}"
    );
}

#[test]
fn the_workspace_summary_carries_the_latest_session() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        "hello",
    );
    fixture.settle(&session);

    match fixture
        .service
        .handle(Request::ListWorkspaces { project: None })
        .unwrap()
    {
        Response::Workspaces { workspaces } => {
            let summary = workspaces
                .iter()
                .find(|summary| summary.id() == fixture.workspace)
                .expect("the workspace is listed");
            assert_eq!(
                summary.session.as_ref().map(|session| session.id.clone()),
                Some(session),
                "the sidebar needs the session without a second request"
            );
        }
        other => panic!("expected workspaces, got {other:?}"),
    }
}

#[test]
fn cancelling_stops_the_agent_rather_than_waiting_for_it() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#,
            "#sleep 60000",
            r#"{"type":"result","subtype":"success","is_error":false}"#,
        ]
        .join("\n"),
        "take your time",
    );

    // Wait until the agent has actually started talking, so the cancel lands
    // on a running process rather than on one that has not spawned yet.
    let deadline = Instant::now() + Duration::from_secs(20);
    while spoken(&fixture.transcript(&session)).is_empty() {
        assert!(Instant::now() < deadline, "the agent never started");
        std::thread::sleep(Duration::from_millis(20));
    }

    fixture
        .service
        .handle(Request::CancelSession {
            session: session.clone(),
        })
        .unwrap();
    assert_eq!(fixture.settle(&session), SessionState::Cancelled);
}

#[test]
fn cancelling_reaches_the_tools_the_agent_started() {
    // An agent's compiler or test runner is a child of the agent, and killing
    // only the agent leaves it running against the worktree.
    let mut fixture = Fixture::new();
    let pidfile = fixture.path("child.pid");
    let session = fixture.start(
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"building"}]}}"#,
            &format!("#spawn {}", pidfile.display()),
            "#sleep 60000",
        ]
        .join("\n"),
        "build it",
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    while !pidfile.exists() {
        assert!(
            Instant::now() < deadline,
            "the agent never started its child"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let child: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(child), "the child is running before the cancel");

    fixture
        .service
        .handle(Request::CancelSession {
            session: session.clone(),
        })
        .unwrap();
    assert_eq!(fixture.settle(&session), SessionState::Cancelled);

    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(child) {
        assert!(
            Instant::now() < deadline,
            "the agent's child survived the cancel"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Whether a pid is still there, asked the way the supervisor asks.
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn two_agents_run_in_two_workspaces_at_once() {
    // The point of worktree-per-task: one workspace's agent must not wait on
    // another's, and neither transcript may pick up the other's events.
    let mut fixture = Fixture::new();
    let second = match fixture
        .service
        .handle(Request::CreateWorkspace {
            project: ProjectName("comet".into()),
            branch: "second".into(),
            base: None,
        })
        .unwrap()
    {
        Response::Workspace { workspace } => workspace.id(),
        other => panic!("expected a workspace, got {other:?}"),
    };

    let script = [
        r#"{"type":"system","subtype":"init","session_id":"v"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[{prompt}]"}]}}"#,
        "#sleep 300",
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
    ]
    .join("\n");
    let first_session = fixture.start(&script, "first workspace");
    let second_session = match fixture
        .service
        .handle(Request::StartSession {
            workspace: second,
            agent: "claude".into(),
            prompt: "second workspace".into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            account: None,
            access_mode: None,
            origin: None,
        })
        .unwrap()
    {
        Response::Session { session } => session.id,
        other => panic!("expected a session, got {other:?}"),
    };

    assert_eq!(fixture.settle(&first_session), SessionState::Finished);
    assert_eq!(fixture.settle(&second_session), SessionState::Finished);
    assert!(spoken(&fixture.transcript(&first_session)).contains("first workspace"));
    let second_text = spoken(&fixture.transcript(&second_session));
    assert!(second_text.contains("second workspace"), "{second_text}");
    assert!(
        !second_text.contains("first workspace"),
        "the transcripts must not mix: {second_text}"
    );
}

#[test]
fn an_agent_that_exits_badly_leaves_a_failed_session() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            "#stderr not authenticated",
            "#exit 3",
        ]
        .join("\n"),
        "hello",
    );
    assert_eq!(fixture.settle(&session), SessionState::Failed);
}

#[test]
fn a_vendor_that_changed_its_format_fails_loudly_rather_than_reporting_silence() {
    // Exit code 0 with output the driver cannot read is the dangerous case: it
    // looks like a session that simply had nothing to say.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &["some new banner", "another unparseable line"].join("\n"),
        "hello",
    );
    assert_eq!(fixture.settle(&session), SessionState::Failed);

    match fixture
        .service
        .handle(Request::ListSessions {
            workspace: None,
            origin: None,
        })
        .unwrap()
    {
        Response::Sessions { sessions } => {
            let summary = sessions[0].summary.clone().unwrap_or_default();
            assert!(
                summary.contains("understand") || summary.contains("version"),
                "the failure has to name the cause: {summary}"
            );
        }
        other => panic!("expected sessions, got {other:?}"),
    }
}

#[test]
fn a_follow_up_sent_while_the_agent_is_busy_runs_as_a_resume_afterwards() {
    // The fallback, for a transport with no way into a running turn: hold the
    // message where the user can still see it, and open a fresh turn once the
    // current one settles (§3.3 N1).
    let mut fixture = Fixture::new();
    let session = fixture.start_with(
        "codex",
        &[
            r#"{"type":"thread.started","thread_id":"vendor-9"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":"[turn:{args}]"}}"#,
            "#sleep 400",
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
        "first",
    );

    // Queued while the first turn is still going.
    fixture
        .service
        .handle(Request::SendMessage {
            session: session.clone(),
            text: "second".into(),
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let transcript = fixture.transcript(&session);
        let turns = transcript
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    TranscriptPayload::Agent {
                        event: AgentEvent::TurnEnd { .. }
                    }
                )
            })
            .count();
        if turns >= 2 {
            let spoken = spoken(&transcript);
            assert!(
                spoken.contains("resume vendor-9"),
                "the follow-up must continue the vendor's session: {spoken}"
            );
            assert_eq!(
                transcript
                    .iter()
                    .filter(|entry| matches!(entry, TranscriptPayload::User { .. }))
                    .count(),
                2,
                "both prompts are in the transcript"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the queued follow-up never ran: {transcript:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_busy_sessions_queue_can_be_read_edited_reordered_and_trimmed() {
    let mut fixture = Fixture::new();
    let session = fixture.start_with(
        "codex",
        &[
            r#"{"type":"thread.started","thread_id":"vendor-q"}"#,
            "#sleep 1200",
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
        "first",
    );
    for text in ["second", "third", "remove me"] {
        fixture.ask(Request::SendMessage {
            session: session.clone(),
            text: text.into(),
        });
    }

    let (queued, can_send_now) = match fixture.ask(Request::QueuedMessages {
        session: session.clone(),
    }) {
        Response::QueuedMessages {
            messages,
            can_send_now,
            ..
        } => (messages, can_send_now),
        other => panic!("expected queued messages, got {other:?}"),
    };
    assert!(
        !can_send_now,
        "Codex exec must advertise that it cannot inject into the live turn"
    );
    assert_eq!(
        queued
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        vec!["second", "third", "remove me"]
    );
    assert_eq!(
        fixture
            .transcript(&session)
            .iter()
            .filter(|entry| matches!(entry, TranscriptPayload::User { .. }))
            .count(),
        1,
        "waiting prompts are queue rows, not immutable transcript entries"
    );

    fixture.ask(Request::EditQueuedMessage {
        session: session.clone(),
        id: queued[1].id,
        text: "edited third".into(),
    });
    fixture.ask(Request::MoveQueuedMessage {
        session: session.clone(),
        id: queued[1].id,
        index: 0,
    });
    fixture.ask(Request::RemoveQueuedMessage {
        session: session.clone(),
        id: queued[2].id,
    });
    let queued = match fixture.ask(Request::QueuedMessages {
        session: session.clone(),
    }) {
        Response::QueuedMessages { messages, .. } => messages,
        other => panic!("expected queued messages, got {other:?}"),
    };
    assert_eq!(
        queued
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        vec!["edited third", "second"]
    );

    let send_now = fixture.service.handle(Request::SendQueuedMessageNow {
        session: session.clone(),
        id: queued[0].id,
    });
    assert!(
        send_now.is_err(),
        "Codex exec cannot receive unsolicited input, so useful work stays running"
    );
    assert_eq!(
        match fixture.ask(Request::QueuedMessages {
            session: session.clone(),
        }) {
            Response::QueuedMessages { messages, .. } => messages.len(),
            other => panic!("expected queued messages, got {other:?}"),
        },
        2,
        "a refused send-now leaves the queue untouched"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let users = fixture
            .transcript(&session)
            .into_iter()
            .filter_map(|entry| match entry {
                TranscriptPayload::User { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        if users.len() >= 2 {
            assert_eq!(users[..2], ["first", "edited third"]);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the reordered prompt never dispatched"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn turns_keep_counting_across_the_processes_that_ran_them() {
    // Each turn is its own process, so a driver's own count restarts every
    // time. A transcript that says "end of turn 1" twice, and two checkpoints
    // both labelled turn 1, describe a conversation that did not happen.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        ]
        .join("\n"),
        "first",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    fixture
        .service
        .handle(Request::SendMessage {
            session: session.clone(),
            text: "second".into(),
        })
        .unwrap();

    // The second turn starts asynchronously, so waiting on the session state
    // would see the first turn's `finished` and stop too early.
    let turns = fixture.wait_for(&session, |turns| turns.len() == 2);
    assert_eq!(turns, vec![1, 2]);

    let mut checkpoint_turns: Vec<u32> = fixture
        .checkpoints()
        .iter()
        .map(|checkpoint| checkpoint.turn)
        .collect();
    checkpoint_turns.sort_unstable();
    assert_eq!(
        checkpoint_turns,
        vec![0, 1, 2],
        "each turn's checkpoint has to be tellable from the others"
    );
}

#[test]
fn a_conversation_is_titled_renameable_and_forgettable() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        "Fix the parser\n\nIt drops the last token.",
    );
    fixture.settle(&session);

    let stored = fixture.stored(&session);
    assert_eq!(
        stored.title.as_deref(),
        Some("Fix the parser"),
        "a conversation is titled by what was asked of it"
    );

    fixture.ask(Request::RenameSession {
        session: session.clone(),
        title: "The tokenizer".into(),
    });
    assert_eq!(
        fixture.stored(&session).title.as_deref(),
        Some("The tokenizer")
    );

    fixture.ask(Request::RemoveSession {
        session: session.clone(),
    });
    match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: None,
    }) {
        Response::Sessions { sessions } => {
            assert!(sessions.iter().all(|listed| listed.id != session))
        }
        other => panic!("expected sessions, got {other:?}"),
    }
}

#[test]
fn a_fork_keeps_the_conversation_up_to_the_point_it_was_taken() {
    // Taking the same work somewhere else without losing where it came from.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first answer"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-1"}"#,
        ]
        .join("\n"),
        "the original",
    );
    fixture.settle(&session);
    let original = fixture.transcript(&session);
    assert!(original.len() > 2);

    // Fork at the agent's first words, dropping everything after.
    let forked = match fixture.ask(Request::ForkSession {
        session: session.clone(),
        after: Some(2),
        agent: None,
        model: None,
        account: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };

    assert_eq!(forked.workspace, fixture.workspace);
    assert_eq!(
        forked.vendor_session_id.as_deref(),
        Some("vendor-1"),
        "continuing a fork continues the agent's own conversation"
    );
    assert_eq!(forked.title.as_deref(), Some("the original (fork)"));

    let copied = fixture.transcript(&forked.id);
    assert_eq!(copied.len(), 2, "up to the point it was forked at");
    assert_eq!(copied[0], original[0], "and it is the same conversation");

    // The original is untouched.
    assert_eq!(fixture.transcript(&session).len(), original.len());
}

#[test]
fn a_fork_onto_another_agent_is_handed_the_record_rather_than_the_thread() {
    // The thread lives in the first vendor's store and cannot be resumed by
    // the second; the daemon's own transcript can be read by anyone. So the
    // fork gets a digest of it in front of its first prompt, once.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I renamed the tokenizer"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-1"}"#,
        ]
        .join("\n"),
        "tidy the parser",
    );
    fixture.settle(&session);

    let forked = match fixture.ask(Request::ForkSession {
        session: session.clone(),
        after: None,
        agent: Some("codex".into()),
        model: None,
        account: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_eq!(forked.agent, "codex");
    assert_eq!(
        forked.vendor_session_id, None,
        "another vendor cannot continue this one's thread"
    );
    assert_eq!(
        forked.title.as_deref(),
        Some("tidy the parser (codex fork)")
    );
    assert_eq!(forked.summary.as_deref(), Some("moved from claude"));
    assert_eq!(
        fixture.transcript(&forked.id).len(),
        fixture.transcript(&session).len(),
        "the record came across whole"
    );

    // The fake codex echoes its arguments, which is where the prompt goes.
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"thread.started","thread_id":"vendor-2"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    fixture.ask(Request::SendMessage {
        session: forked.id.clone(),
        text: "carry on".into(),
    });
    let transcript = fixture.wait_for_said(&forked.id, "carry on");
    let told = spoken(&transcript);
    assert!(
        told.contains("moved to you from another agent (claude)"),
        "the digest went to the agent: {told}"
    );
    assert!(told.contains("tidy the parser"), "{told}");
    assert!(told.contains("claude: I renamed the tokenizer"), "{told}");
    assert!(told.contains("carry on"), "{told}");
    assert!(!told.contains("resume"), "nothing to resume: {told}");
    // The reader's transcript shows what they typed, not what the agent was
    // told in front of it.
    let typed: Vec<&str> = transcript
        .iter()
        .filter_map(|entry| match entry {
            TranscriptPayload::User { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(typed.last().copied(), Some("carry on"));
    assert!(typed.iter().all(|text| !text.contains("moved to you")));

    // Once the agent has a thread of its own the digest is spent.
    assert_eq!(
        fixture.stored(&forked.id).vendor_session_id.as_deref(),
        Some("vendor-2")
    );
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"thread.started","thread_id":"vendor-2"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    fixture.ask(Request::SendMessage {
        session: forked.id.clone(),
        text: "and again".into(),
    });
    let again = spoken(&fixture.wait_for_said(&forked.id, "and again"));
    let second = &again[again
        .find("resume vendor-2")
        .expect("the second turn resumed")..];
    assert!(second.contains("and again"), "{second}");
    assert!(!second.contains("moved to you"), "sent once: {second}");
}

#[test]
fn a_commit_message_is_written_by_an_agent_and_pushed_when_it_lands() {
    // N9: one read-only turn on the cheap tier, answered as an event rather
    // than inline, because a model's thirty seconds must not hold every other
    // client's next request.
    let mut fixture = Fixture::new();
    let worktree = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces
            .into_iter()
            .find(|summary| summary.id() == fixture.workspace)
            .map(|summary| summary.worktree.path)
            .unwrap(),
        other => panic!("expected workspaces, got {other:?}"),
    };
    std::fs::write(worktree.join("parser.rs"), "fn parse() {}\n").unwrap();
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"system","subtype":"init","session_id":"one-shot"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"\"Add a parser stub\"\n\nIt parses nothing yet."}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"one-shot"}"#,
        ]
        .join("\n"),
    )
    .unwrap();

    match fixture.ask(Request::GenerateCommitMessage {
        workspace: fixture.workspace.clone(),
        agent: Some("claude".into()),
        staged: false,
    }) {
        Response::Ack => {}
        other => panic!("expected an ack, got {other:?}"),
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    let (message, error) = loop {
        let found = fixture
            .recorder
            .all()
            .into_iter()
            .find_map(|event| match event {
                DaemonEvent::CommitMessageGenerated { message, error, .. } => {
                    Some((message, error))
                }
                _ => None,
            });
        if let Some(found) = found {
            break found;
        }
        assert!(Instant::now() < deadline, "no message was ever pushed");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(error, None);
    assert_eq!(
        message.as_deref(),
        Some("Add a parser stub\n\nIt parses nothing yet.\n"),
        "unquoted, subject and body apart, as git takes it"
    );
    // No session was made for it: a one-shot is not a conversation.
    match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: None,
    }) {
        Response::Sessions { sessions } => assert!(sessions.is_empty()),
        other => panic!("expected sessions, got {other:?}"),
    }
}

#[test]
fn a_clean_worktree_has_no_commit_message_to_write() {
    let mut fixture = Fixture::new();
    fixture.ask(Request::GenerateCommitMessage {
        workspace: fixture.workspace.clone(),
        agent: Some("claude".into()),
        staged: false,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(error) = fixture
            .recorder
            .all()
            .into_iter()
            .find_map(|event| match event {
                DaemonEvent::CommitMessageGenerated { error, .. } => Some(error),
                _ => None,
            })
        {
            assert!(error.unwrap_or_default().contains("clean"));
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_sessions_access_mode_is_kept_and_a_follow_up_runs_under_it() {
    // N2: the mode is a launch argument on every transport, so a follow-up
    // has to be started with the one the conversation was, not the default.
    let mut fixture = Fixture::new();
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let session = match fixture.ask(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "codex".into(),
        prompt: "look only".into(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        account: None,
        access_mode: Some(ginka_protocol::AccessMode::ReadOnly),
        origin: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_eq!(session.access_mode, ginka_protocol::AccessMode::ReadOnly);
    let told = spoken(&fixture.wait_for_said(&session.id, "look only"));
    assert!(told.contains("--sandbox read-only"), "{told}");

    fixture.ask(Request::SendMessage {
        session: session.id.clone(),
        text: "and again".into(),
    });
    let told = spoken(&fixture.wait_for_said(&session.id, "and again"));
    let second = &told[told.find("resume t-1").expect("resumed")..];
    assert!(
        second.contains("--sandbox read-only"),
        "the same mode: {second}"
    );
    assert_eq!(
        fixture.stored(&session.id).access_mode,
        ginka_protocol::AccessMode::ReadOnly,
        "and it is what the record says"
    );

    // Unnamed, the mode is `ask`, which on Codex is what `exec` does on its
    // own: nothing is said about the sandbox.
    let plain = match fixture.ask(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "codex".into(),
        prompt: "just edit".into(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        account: None,
        access_mode: None,
        origin: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_eq!(plain.access_mode, ginka_protocol::AccessMode::Ask);
    let told = spoken(&fixture.wait_for_said(&plain.id, "just edit"));
    assert!(
        !told.contains("--sandbox") && !told.contains("--full-auto"),
        "{told}"
    );
}

#[test]
fn a_sessions_reasoning_and_tier_are_kept_and_reused_on_follow_up() {
    let mut fixture = Fixture::new();
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"thread.started","thread_id":"t-options"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let session = match fixture.ask(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "codex".into(),
        prompt: "think carefully".into(),
        model: Some("gpt-next".into()),
        reasoning_effort: Some("high".into()),
        service_tier: Some("priority".into()),
        account: None,
        access_mode: None,
        origin: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_eq!(session.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(session.service_tier.as_deref(), Some("priority"));
    let told = spoken(&fixture.wait_for_said(&session.id, "think carefully"));
    assert!(told.contains("model_reasoning_effort=\"high\""), "{told}");
    assert!(told.contains("service_tier=\"priority\""), "{told}");

    fixture.ask(Request::SendMessage {
        session: session.id.clone(),
        text: "continue".into(),
    });
    let told = spoken(&fixture.wait_for_said(&session.id, "continue"));
    let second = &told[told.find("resume t-options").expect("resumed")..];
    assert!(
        second.contains("model_reasoning_effort=\"high\""),
        "{second}"
    );
    assert!(second.contains("service_tier=\"priority\""), "{second}");
}

#[test]
fn absorbed_options_update_the_next_turn_of_an_existing_session() {
    let mut fixture = Fixture::new();
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"thread.started","thread_id":"t-update-options"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            "#sleep 300",
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let session = match fixture.ask(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "codex".into(),
        prompt: "begin".into(),
        model: Some("gpt-old".into()),
        reasoning_effort: Some("low".into()),
        service_tier: None,
        account: None,
        access_mode: None,
        origin: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    fixture.wait_for_said(&session.id, "begin");

    let updated = match fixture.ask(Request::UpdateSessionOptions {
        session: session.id.clone(),
        model: Some("gpt-next".into()),
        reasoning_effort: Some("high".into()),
        service_tier: Some("priority".into()),
    }) {
        Response::SessionOptionsApplied { session, outcome } => {
            assert_eq!(outcome, ginka_protocol::OptionOutcome::Absorbed);
            session
        }
        other => panic!("expected updated options, got {other:?}"),
    };
    assert_eq!(updated.model.as_deref(), Some("gpt-next"));
    assert_eq!(updated.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(updated.service_tier.as_deref(), Some("priority"));

    fixture.ask(Request::SendMessage {
        session: session.id.clone(),
        text: "continue with the new options".into(),
    });
    let told = spoken(&fixture.wait_for_said(&session.id, "continue with the new options"));
    let second = &told[told.find("resume t-update-options").expect("resumed")..];
    assert!(second.contains("--model gpt-next"), "{second}");
    assert!(
        second.contains("model_reasoning_effort=\"high\""),
        "{second}"
    );
    assert!(second.contains("service_tier=\"priority\""), "{second}");

    let reset = match fixture.ask(Request::UpdateSessionOptions {
        session: session.id.clone(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
    }) {
        Response::SessionOptionsApplied { session, outcome } => {
            assert_eq!(outcome, ginka_protocol::OptionOutcome::Absorbed);
            session
        }
        other => panic!("expected reset options, got {other:?}"),
    };
    assert_eq!(reset.model, None);
    assert_eq!(reset.reasoning_effort, None);
    assert_eq!(reset.service_tier, None);
    fixture.ask(Request::SendMessage {
        session: session.id.clone(),
        text: "continue with provider defaults".into(),
    });
    let told = spoken(&fixture.wait_for_said(&session.id, "continue with provider defaults"));
    let third = &told[told.rfind("resume t-update-options").expect("resumed")..];
    assert!(!third.contains("--model"), "{third}");
    assert!(!third.contains("model_reasoning_effort"), "{third}");
    assert!(!third.contains("service_tier"), "{third}");
}

#[test]
fn restart_required_replaces_the_session_and_hands_its_context_forward() {
    let mut fixture = Fixture::new();
    let origin = ginka_protocol::model::SessionOrigin {
        connector: "test-chat".into(),
        channel: "channel-1".into(),
        thread: "thread-1".into(),
    };
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"thread.started","thread_id":"old-provider-thread"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let original = match fixture.ask(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "restart-codex".into(),
        prompt: "remember the original task".into(),
        model: Some("gpt-old".into()),
        reasoning_effort: Some("low".into()),
        service_tier: None,
        account: None,
        access_mode: None,
        origin: Some(origin.clone()),
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert!(matches!(
        fixture.settle(&original.id),
        SessionState::Idle | SessionState::Finished
    ));

    let replacement = match fixture.ask(Request::UpdateSessionOptions {
        session: original.id.clone(),
        model: Some("gpt-next".into()),
        reasoning_effort: Some("high".into()),
        service_tier: Some("priority".into()),
    }) {
        Response::SessionOptionsApplied { session, outcome } => {
            assert_eq!(outcome, ginka_protocol::OptionOutcome::RestartRequired);
            session
        }
        other => panic!("expected a replacement session, got {other:?}"),
    };
    assert_ne!(replacement.id, original.id);
    assert_eq!(replacement.vendor_session_id, None);
    assert_eq!(replacement.model.as_deref(), Some("gpt-next"));
    assert_eq!(replacement.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(replacement.service_tier.as_deref(), Some("priority"));
    let active = match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: Some(origin),
    }) {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, replacement.id);

    fixture.ask(Request::SendMessage {
        session: replacement.id.clone(),
        text: "continue after replacement".into(),
    });
    let told = spoken(&fixture.wait_for_said(&replacement.id, "continue after replacement"));
    assert!(!told.contains("resume old-provider-thread"), "{told}");
    assert!(told.contains("remember the original task"), "{told}");
    assert!(told.contains("continue after replacement"), "{told}");
    assert!(told.contains("--model gpt-next"), "{told}");
}

#[test]
fn every_agent_is_handed_ginkas_bridge_and_the_servers_the_user_listed() {
    // Rule 3's third client is only real if the agent is told about it.
    // The settings file names one more server, and it arrives the same way.
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(home.path().join("state"));
    paths.ensure().unwrap();
    std::fs::write(
        paths.daemon_settings(),
        r#"{"tools": {"servers": {"docs": {"command": "npx", "args": ["-y", "docs-mcp"]}}}}"#,
    )
    .unwrap();
    let script = work.path().join("agent-script.txt");
    std::fs::write(
        &script,
        [
            r#"{"type":"thread.started","thread_id":"t"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":{args_json}}}"#,
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let mut drivers = Registry::empty();
    drivers.insert(Arc::new(
        ginka_core::driver::codex::CodexDriver::with_program(FAKE_AGENT)
            .with_exec()
            .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
    ));
    let mut service = Service::new(
        paths.clone(),
        db::open_in_memory().unwrap(),
        Arc::new(Recorder::default()),
    )
    .with_drivers(drivers)
    .with_cli("/opt/ginka/bin/ginka");
    let root = work.path().join("comet");
    support::repository(&root);
    service
        .handle(Request::AddProject {
            path: root,
            label: None,
        })
        .unwrap();
    let workspace = match service
        .handle(Request::CreateWorkspace {
            project: ProjectName("comet".into()),
            branch: "harbor".into(),
            base: None,
        })
        .unwrap()
    {
        Response::Workspace { workspace } => workspace.id(),
        other => panic!("expected a workspace, got {other:?}"),
    };
    let session = match service
        .handle(Request::StartSession {
            workspace,
            agent: "codex".into(),
            prompt: "hello".into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            account: None,
            access_mode: None,
            origin: None,
        })
        .unwrap()
    {
        Response::Session { session } => session.id,
        other => panic!("expected a session, got {other:?}"),
    };

    let deadline = Instant::now() + Duration::from_secs(30);
    let told = loop {
        let transcript = match service
            .handle(Request::SessionTranscript {
                session: session.clone(),
                after: None,
                limit: None,
            })
            .unwrap()
        {
            Response::Transcript { entries } => entries
                .into_iter()
                .map(|entry| entry.payload)
                .collect::<Vec<_>>(),
            other => panic!("expected a transcript, got {other:?}"),
        };
        let told = spoken(&transcript);
        if told.contains("hello") {
            break told;
        }
        assert!(
            Instant::now() < deadline,
            "the agent never ran: {transcript:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        told.contains(r#"mcp_servers.ginka.command="/opt/ginka/bin/ginka""#),
        "{told}"
    );
    assert!(told.contains("mcp_servers.ginka.args=[\"mcp\"]"), "{told}");
    assert!(
        told.contains(&format!(
            "mcp_servers.ginka.env={{GINKA_HOME=\"{}\"}}",
            paths.root().display()
        )),
        "the bridge is pointed at this daemon's state: {told}"
    );
    assert!(told.contains(r#"mcp_servers.docs.command="npx""#), "{told}");
}

#[test]
fn a_fork_onto_an_agent_this_build_does_not_have_is_refused() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        "hello",
    );
    fixture.settle(&session);
    let error = fixture
        .service
        .handle(Request::ForkSession {
            session,
            after: None,
            agent: Some("hal".into()),
            model: None,
            account: None,
        })
        .unwrap_err();
    assert!(error.message.contains("hal"), "{}", error.message);
}

#[test]
fn a_transcript_can_be_searched_for_what_was_said() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"the tokenizer drops the last token"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        ]
        .join("\n"),
        "look into the parser",
    );
    fixture.settle(&session);

    match fixture.ask(Request::SearchSessions {
        workspace: None,
        query: "tokenizer".into(),
        limit: None,
    }) {
        Response::SessionMatches { matches } => {
            let found = matches.first().expect("the agent said it");
            assert_eq!(found.session, session);
            assert!(
                found.excerpt.contains("tokenizer"),
                "the excerpt is what was said, not the json it is stored in: {}",
                found.excerpt
            );
            assert!(found.seq > 0);
        }
        other => panic!("expected matches, got {other:?}"),
    }

    // And the user's own words are searchable too.
    match fixture.ask(Request::SearchSessions {
        workspace: Some(fixture.workspace.clone()),
        query: "look into".into(),
        limit: None,
    }) {
        Response::SessionMatches { matches } => assert!(!matches.is_empty()),
        other => panic!("expected matches, got {other:?}"),
    }

    // A query that matches nothing is an empty answer, not an error.
    match fixture.ask(Request::SearchSessions {
        workspace: None,
        query: "nothing said this".into(),
        limit: None,
    }) {
        Response::SessionMatches { matches } => assert!(matches.is_empty()),
        other => panic!("expected matches, got {other:?}"),
    }
}

#[test]
fn what_a_turn_cost_is_kept_rather_than_watched_and_forgotten() {
    // Every driver already reports it; until it was stored, the only place it
    // existed was a view model that a window closing threw away.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v","total_cost_usd":0.042,"usage":{"input_tokens":1200,"output_tokens":300,"cache_read_input_tokens":900}}"#,
        ]
        .join("\n"),
        "count this",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    match fixture.ask(Request::Usage { days: Some(30) }) {
        Response::Usage {
            by_day,
            by_agent,
            by_account,
            plans,
            ..
        } => {
            let day = by_day.first().expect("it happened today");
            assert_eq!(day.totals.input_tokens, 1200);
            assert_eq!(day.totals.output_tokens, 300);
            assert_eq!(day.totals.cache_read_tokens, 900);
            assert_eq!(day.totals.cost_usd, Some(0.042));

            let agent = by_agent
                .iter()
                .find(|row| row.label == "claude")
                .expect("filed against the agent that ran it");
            assert_eq!(agent.totals.input_tokens, 1200);

            // Nothing chose a login, so the provider's default paid for it.
            let account = by_account
                .iter()
                .find(|row| row.label == "claude")
                .expect("filed against the login that paid for it");
            assert_eq!(account.totals.input_tokens, 1200);
            assert!(plans.is_empty(), "the fake agent reports no windows");
        }
        other => panic!("expected usage, got {other:?}"),
    }
}

#[test]
fn a_review_goes_back_to_the_agent_as_one_message() {
    // M3's whole point: read the diff, mark what is wrong, and let the agent
    // fix it — rather than re-prompting from scratch and throwing away the
    // reading.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[{args}]"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        ]
        .join("\n"),
        "write the parser",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    for (path, line, text) in [
        ("src/main.rs", Some(12), "this unwrap can panic"),
        ("src/main.rs", Some(48), "same here"),
        ("README.md", None, "out of date"),
    ] {
        fixture.ask(Request::AddReviewComment {
            workspace: fixture.workspace.clone(),
            path: path.into(),
            line,
            side: ginka_protocol::DiffSide::New,
            text: text.into(),
        });
    }

    match fixture.ask(Request::ListReviewComments {
        workspace: fixture.workspace.clone(),
    }) {
        Response::ReviewComments { comments } => assert_eq!(comments.len(), 3),
        other => panic!("expected comments, got {other:?}"),
    }

    fixture.ask(Request::SendReviewComments {
        workspace: fixture.workspace.clone(),
        session: session.clone(),
    });

    // One message, carrying every comment, in reading order.
    let sent: Vec<String> = fixture
        .transcript(&session)
        .into_iter()
        .filter_map(|entry| match entry {
            TranscriptPayload::User { text } => Some(text),
            _ => None,
        })
        .collect();
    let batch = sent.last().expect("the review was sent");
    assert!(
        batch.contains("src/main.rs:12 — this unwrap can panic"),
        "{batch}"
    );
    assert!(batch.contains("src/main.rs:48 — same here"), "{batch}");
    assert!(batch.contains("README.md — out of date"), "{batch}");
    assert_eq!(sent.len(), 2, "one message, not one per comment: {sent:?}");

    // And the batch is spent.
    match fixture.ask(Request::ListReviewComments {
        workspace: fixture.workspace.clone(),
    }) {
        Response::ReviewComments { comments } => assert!(comments.is_empty()),
        other => panic!("expected comments, got {other:?}"),
    }
}

#[test]
fn sending_a_review_with_nothing_in_it_says_so() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        "nothing to review",
    );
    fixture.settle(&session);

    let error = fixture
        .service
        .handle(Request::SendReviewComments {
            workspace: fixture.workspace.clone(),
            session,
        })
        .expect_err("there is nothing to send");
    assert!(error.message.contains("no comments"), "{}", error.message);
}

#[test]
fn starting_a_session_in_a_workspace_that_does_not_exist_is_not_found() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::StartSession {
            workspace: WorkspaceId("comet/absent".into()),
            agent: "claude".into(),
            prompt: "hello".into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            account: None,
            access_mode: None,
            origin: None,
        })
        .expect_err("there is no such workspace");
    assert_eq!(error.code, "not_found");
}

#[test]
fn asking_for_an_agent_this_build_does_not_have_names_the_ones_it_does() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::StartSession {
            workspace: fixture.workspace.clone(),
            agent: "telepath".into(),
            prompt: "hello".into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            account: None,
            access_mode: None,
            origin: None,
        })
        .expect_err("there is no such agent");
    assert_eq!(error.code, "not_found");
    assert!(error.message.contains("claude"), "{}", error.message);
}

#[test]
fn every_turn_leaves_a_checkpoint_the_workspace_can_be_rewound_to() {
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"renamed the parser"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#,
        ]
        .join("\n"),
        "rename the parser",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    let checkpoints = fixture.checkpoints();
    assert_eq!(
        checkpoints.len(),
        2,
        "one before the agent started and one at the turn boundary: {checkpoints:?}"
    );
    assert!(
        checkpoints
            .iter()
            .any(|point| point.turn == 0 && point.label.starts_with("before: rename the parser")),
        "there has to be a state from before the agent touched anything: {checkpoints:?}"
    );
    assert!(
        checkpoints
            .iter()
            .any(|point| point.turn == 1 && point.label == "renamed the parser"),
        "a checkpoint is labelled with what the agent said: {checkpoints:?}"
    );
}

#[test]
fn restoring_a_checkpoint_rewinds_the_worktree_and_keeps_what_it_replaced() {
    let mut fixture = Fixture::new();
    let path = fixture.workspace_path();
    std::fs::write(path.join("README.md"), "the good version\n").unwrap();

    let session = fixture.start(
        &[r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v"}"#].join("\n"),
        "leave it alone",
    );
    fixture.settle(&session);

    // Whatever happened next was a mistake.
    std::fs::write(path.join("README.md"), "the bad version\n").unwrap();
    std::fs::write(path.join("regret.txt"), "should not survive\n").unwrap();

    let before_the_agent = fixture
        .checkpoints()
        .into_iter()
        .find(|point| point.turn == 0)
        .expect("the pre-flight checkpoint");
    fixture
        .service
        .handle(Request::RestoreCheckpoint {
            checkpoint: before_the_agent.id.clone(),
        })
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "the good version\n"
    );
    assert!(!path.join("regret.txt").exists());
    assert!(
        fixture
            .checkpoints()
            .iter()
            .any(|point| point.label.starts_with("before restoring:")),
        "the state a rewind replaced has to be reachable too"
    );
}

#[test]
fn restoring_a_checkpoint_that_does_not_exist_is_not_found() {
    let mut fixture = Fixture::new();
    let error = fixture
        .service
        .handle(Request::RestoreCheckpoint {
            checkpoint: ginka_protocol::CheckpointId("absent".into()),
        })
        .expect_err("there is no such checkpoint");
    assert_eq!(error.code, "not_found");
}

#[test]
fn a_fan_out_asks_the_same_question_in_a_worktree_each() {
    // Orca's idea: a task with more than one reasonable approach is worth
    // trying more than once, and the attempts must not tread on each other.
    let mut fixture = Fixture::new();
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"on it"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"on it","session_id":"vendor-1"}"#,
        ]
        .join("\n"),
    )
    .unwrap();

    let (started, failed) = match fixture
        .service
        .handle(Request::FanOut {
            project: ProjectName("comet".into()),
            branch_prefix: "attempt".into(),
            base: None,
            prompt: "make it faster".into(),
            attempts: vec![
                ginka_protocol::rpc::Attempt {
                    agent: "claude".into(),
                    model: None,
                    account: None,
                },
                ginka_protocol::rpc::Attempt {
                    agent: "claude".into(),
                    model: None,
                    account: None,
                },
                // A driver this build does not have: the arm fails and the
                // others carry on.
                ginka_protocol::rpc::Attempt {
                    agent: "no-such-agent".into(),
                    model: None,
                    account: None,
                },
            ],
        })
        .unwrap()
    {
        Response::FannedOut { started, failed } => (started, failed),
        other => panic!("expected a fan-out, got {other:?}"),
    };

    assert_eq!(started.len(), 2, "two arms started: {failed:?}");
    assert_eq!(failed.len(), 1, "{failed:?}");
    let workspaces: Vec<String> = started
        .iter()
        .map(|session| session.workspace.0.clone())
        .collect();
    assert_eq!(
        workspaces,
        vec!["comet/attempt-1", "comet/attempt-2"],
        "one worktree each, named in order"
    );

    for session in &started {
        assert_eq!(
            fixture.settle(&session.id),
            SessionState::Finished,
            "every arm runs its own agent"
        );
    }
}

#[test]
fn an_attachment_reaches_the_agent_as_a_path_it_can_open() {
    // The user attaches a file and mentions it; the agent gets somewhere to
    // read, because that is what an agent can act on (§3.3 N6).
    use base64::Engine as _;
    let mut fixture = Fixture::new();
    let attachment = match fixture
        .service
        .handle(Request::UploadAttachment {
            name: "notes.md".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"# hello"),
        })
        .unwrap()
    {
        Response::Attachment { attachment } => attachment,
        other => panic!("expected an attachment, got {other:?}"),
    };

    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"{prompt}"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false}"#,
        ]
        .join("\n"),
        &format!("summarise {}", attachment.reference),
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    let echoed = format!("{:?}", fixture.transcript(&session));
    assert!(
        echoed.contains("attachments/"),
        "the agent was handed a path, not a reference: {echoed}"
    );
}

#[test]
fn a_follow_up_reaches_the_turn_that_is_already_running() {
    // Steering, and the reason the composer does not simply hold everything
    // until the agent stops: on a transport that can take a message mid-turn,
    // "stop, retype, resend" is friction with nothing behind it (§3.3 N1).
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[first:{prompt}]"}]}}"#,
            "#read",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[steered:{stdin}]"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-1"}"#,
        ]
        .join("\n"),
        "first",
    );

    // While the turn is still going: the script is blocked on `#read`.
    fixture
        .service
        .handle(Request::SendMessage {
            session: session.clone(),
            text: "actually, use serde_json".into(),
        })
        .unwrap();

    let settled = fixture.settle(&session);
    let transcript = fixture.transcript(&session);
    let spoken = format!("{transcript:?}");
    assert_eq!(settled, SessionState::Finished, "{spoken}");

    assert!(
        spoken.contains("actually, use serde_json"),
        "the steered message never reached the agent: {spoken}"
    );
    assert!(
        spoken.contains("[first:first]"),
        "the turn it was steered into is the one that was running: {spoken}"
    );

    let turns = transcript
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                TranscriptPayload::Agent {
                    event: AgentEvent::TurnEnd { .. }
                }
            )
        })
        .count();
    assert_eq!(turns, 1, "steering does not open a second turn: {spoken}");
}

#[test]
fn a_steered_message_is_in_the_transcript_like_any_other() {
    // The reader has to see what they sent, whichever way it was delivered.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"vendor-2"}"#,
            "#read",
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-2"}"#,
        ]
        .join("\n"),
        "first",
    );
    fixture
        .service
        .handle(Request::SendMessage {
            session: session.clone(),
            text: "and the changelog".into(),
        })
        .unwrap();
    fixture.settle(&session);

    let users: Vec<String> = fixture
        .transcript(&session)
        .into_iter()
        .filter_map(|entry| match entry {
            TranscriptPayload::User { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(users, ["first", "and the changelog"]);
}

// ---------------------------------------------------------------------------
// Accounts: several logins per provider (`docs/accounts.md`).

impl Fixture {
    /// Everything the agent said in a session, joined.
    fn text_of(&mut self, id: &SessionId) -> String {
        self.transcript(id)
            .into_iter()
            .filter_map(|payload| match payload {
                TranscriptPayload::Agent {
                    event: AgentEvent::TextDelta { text },
                } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

fn accounts_of(fixture: &mut Fixture) -> Vec<ginka_protocol::model::Account> {
    match fixture.ask(Request::Accounts) {
        Response::Accounts { accounts } => accounts,
        other => panic!("expected accounts, got {other:?}"),
    }
}

#[test]
fn a_session_on_a_named_account_runs_with_that_accounts_directory() {
    let mut fixture = Fixture::new();
    let added = fixture.ask(Request::AddAccount {
        id: ginka_protocol::AccountId("claude-work".into()),
        provider: ginka_protocol::ProviderKind::Claude,
        label: "Work".into(),
    });
    let Response::Account { account } = added else {
        panic!("expected the account, got {added:?}");
    };
    let home = account
        .home
        .clone()
        .expect("a named account has a directory");
    assert!(home.is_dir(), "created for the vendor to sign into");
    assert!(
        fixture
            .recorder
            .all()
            .contains(&DaemonEvent::AccountsChanged)
    );
    fixture.ask(Request::SelectAccount {
        id: ginka_protocol::AccountId("claude-work".into()),
    });
    let accounts = accounts_of(&mut fixture);
    assert!(
        accounts
            .iter()
            .any(|account| { account.id.0 == "claude-work" && account.active })
    );
    assert!(
        accounts
            .iter()
            .any(|account| { account.id.0 == "claude" && !account.active })
    );

    // The agent says what it was given; the script is the same for both.
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"system","subtype":"init","session_id":"v"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"home={env:CLAUDE_CONFIG_DIR}"}]},"session_id":"v"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"v","usage":{"input_tokens":7,"output_tokens":1}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let on_work = match fixture.service.handle(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "claude".into(),
        prompt: "where are you".into(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        account: None,
        access_mode: None,
        origin: None,
    }) {
        Ok(Response::Session { session }) => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_eq!(on_work.account.0, "claude-work");
    assert_eq!(fixture.settle(&on_work.id), SessionState::Finished);
    let said = fixture.text_of(&on_work.id);
    assert!(
        said.contains(&format!("home={}", home.display())),
        "the account's directory reached the agent: {said}"
    );

    // Switching changes the default for new conversations. The existing
    // record stays on the account that owns its vendor thread.
    fixture.ask(Request::SelectAccount {
        id: ginka_protocol::AccountId("claude".into()),
    });
    let stored = match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: None,
    }) {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    let original = stored
        .iter()
        .find(|session| session.id == on_work.id)
        .expect("the first conversation remains listed");
    assert_eq!(original.account.0, "claude-work");

    // And the active default account points the CLI nowhere in particular:
    // its variable is not set at all.
    let on_default = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"d"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"home=[{env:CLAUDE_CONFIG_DIR}]"}]},"session_id":"d"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"d","usage":{"input_tokens":3,"output_tokens":1}}"#,
        ]
        .join("\n"),
        "where are you now",
    );
    assert_eq!(fixture.settle(&on_default), SessionState::Finished);
    assert!(fixture.text_of(&on_default).contains("home=[]"));
    let stored = match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: None,
    }) {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    let default = stored
        .iter()
        .find(|session| session.id == on_default)
        .unwrap();
    assert_eq!(default.account.0, "claude", "the provider's own id");

    // The cost is filed against the login that paid it.
    match fixture.ask(Request::Usage { days: Some(30) }) {
        Response::Usage { by_account, .. } => {
            let work = by_account
                .iter()
                .find(|row| row.label == "claude-work")
                .expect("the work login paid for its turn");
            assert_eq!(work.totals.input_tokens, 7);
        }
        other => panic!("expected usage, got {other:?}"),
    }
}

#[test]
fn a_login_of_one_provider_cannot_run_another_providers_agent() {
    let mut fixture = Fixture::new();
    fixture.ask(Request::AddAccount {
        id: ginka_protocol::AccountId("codex-work".into()),
        provider: ginka_protocol::ProviderKind::Codex,
        label: "Work".into(),
    });
    let refused = fixture.service.handle(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "claude".into(),
        prompt: "hello".into(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        account: Some(ginka_protocol::AccountId("codex-work".into())),
        access_mode: None,
        origin: None,
    });
    let error = refused.expect_err("a codex login is not a claude one");
    assert!(error.message.contains("codex"), "{}", error.message);

    let unknown = fixture.service.handle(Request::StartSession {
        workspace: fixture.workspace.clone(),
        agent: "claude".into(),
        prompt: "hello".into(),
        model: None,
        reasoning_effort: None,
        service_tier: None,
        account: Some(ginka_protocol::AccountId("nonesuch".into())),
        access_mode: None,
        origin: None,
    });
    assert!(unknown.is_err());
    // Nothing was started for either.
    match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: None,
    }) {
        Response::Sessions { sessions } => assert!(sessions.is_empty()),
        other => panic!("expected sessions, got {other:?}"),
    }
}

#[test]
fn the_defaults_are_always_listed_and_a_named_account_can_be_forgotten() {
    let mut fixture = Fixture::new();
    let ids = |accounts: &[ginka_protocol::model::Account]| {
        accounts
            .iter()
            .map(|account| account.id.0.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&accounts_of(&mut fixture)),
        vec!["claude", "codex", "gemini", "opencode"]
    );

    fixture.ask(Request::AddAccount {
        id: ginka_protocol::AccountId("codex-work".into()),
        provider: ginka_protocol::ProviderKind::Codex,
        label: "Work".into(),
    });
    let accounts = accounts_of(&mut fixture);
    assert_eq!(
        ids(&accounts),
        vec!["claude", "codex", "codex-work", "gemini", "opencode"]
    );
    let work = &accounts[2];
    assert_eq!(work.label, "Work");
    assert!(!work.is_default);
    let login = work.login.as_ref().expect("codex has a sign-in command");
    assert_eq!(login.args, vec!["login"]);
    assert_eq!(login.env[0].0, "CODEX_HOME");

    fixture.ask(Request::SelectAccount {
        id: ginka_protocol::AccountId("codex-work".into()),
    });
    assert!(
        accounts_of(&mut fixture)
            .iter()
            .any(|account| account.id.0 == "codex-work" && account.active)
    );

    // Ids are slugs and never a provider's own.
    assert!(
        fixture
            .service
            .handle(Request::AddAccount {
                id: ginka_protocol::AccountId("claude".into()),
                provider: ginka_protocol::ProviderKind::Claude,
                label: "Again".into(),
            })
            .is_err()
    );
    assert!(
        fixture
            .service
            .handle(Request::RemoveAccount {
                id: ginka_protocol::AccountId("claude".into()),
                delete_home: false,
            })
            .is_err()
    );

    let home = work.home.clone().unwrap();
    fixture.ask(Request::RemoveAccount {
        id: ginka_protocol::AccountId("codex-work".into()),
        delete_home: false,
    });
    assert_eq!(
        ids(&accounts_of(&mut fixture)),
        vec!["claude", "codex", "gemini", "opencode"]
    );
    assert!(
        accounts_of(&mut fixture)
            .iter()
            .any(|account| account.id.0 == "codex" && account.active)
    );
    assert!(home.is_dir(), "the vendor's login is kept unless asked");

    // And what was written survives a daemon: it is in the settings file.
    let settings: ginka_core::settings::DaemonSettings =
        ginka_core::settings::load(&fixture.service.paths().daemon_settings());
    assert!(settings.accounts.is_empty());
    assert!(settings.active_accounts.is_empty());
}

#[test]
fn the_windows_a_turn_reports_are_kept_per_account_and_pushed() {
    let mut fixture = Fixture::new();
    // The older Codex stream, which carries the account's windows beside the
    // token counts.
    let session = fixture.start_with(
        "codex",
        &[
            r#"{"id":"0","msg":{"type":"session_configured","session_id":"c1"}}"#,
            r#"{"id":"1","msg":{"type":"agent_message","message":"Hello"}}"#,
            r#"{"id":"2","msg":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5,"output_tokens":1}},"rate_limits":{"primary":{"used_percent":92,"window_minutes":300,"resets_at":1900000000},"secondary":{"used_percent":40,"window_minutes":10080,"resets_at":1900600000}}}}"#,
            r#"{"id":"3","msg":{"type":"task_complete","last_agent_message":"Hello"}}"#,
        ]
        .join("\n"),
        "how much is left",
    );
    assert_eq!(fixture.settle(&session), SessionState::Finished);

    let pushed = fixture
        .recorder
        .all()
        .into_iter()
        .find_map(|event| match event {
            DaemonEvent::PlanUsageChanged { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("the gauge moved, and every client was told");
    assert_eq!(pushed.account.0, "codex");
    assert_eq!(pushed.source, ginka_protocol::model::PlanSource::Reported);
    assert_eq!(
        pushed.usage.tightest().map(|window| window.label.as_str()),
        Some("5h")
    );

    match fixture.ask(Request::Usage { days: Some(30) }) {
        Response::Usage { plans, .. } => {
            assert_eq!(plans, vec![pushed]);
        }
        other => panic!("expected usage, got {other:?}"),
    }
    // A gauge is not conversation: the transcript does not carry it.
    assert!(!fixture.transcript(&session).iter().any(|payload| matches!(
        payload,
        TranscriptPayload::Agent {
            event: AgentEvent::PlanUsage { .. }
        }
    )));
}

/// A Codex session that is busy long enough to queue behind, with two
/// follow-ups waiting.
fn busy_codex_with_two_queued(fixture: &mut Fixture) -> SessionId {
    let session = fixture.start_with(
        "codex",
        &[
            r#"{"type":"thread.started","thread_id":"vendor-p"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","item_type":"agent_message","text":"[turn:{args}]"}}"#,
            "#sleep 1500",
            r#"{"id":"1","msg":{"type":"task_complete","last_agent_message":"done"}}"#,
        ]
        .join("\n"),
        "first",
    );
    for text in ["second", "third"] {
        fixture.ask(Request::SendMessage {
            session: session.clone(),
            text: text.into(),
        });
    }
    session
}

fn queue_of(fixture: &mut Fixture, session: &SessionId) -> (Vec<String>, bool) {
    match fixture.ask(Request::QueuedMessages {
        session: session.clone(),
    }) {
        Response::QueuedMessages {
            messages, paused, ..
        } => (messages.into_iter().map(|m| m.text).collect(), paused),
        other => panic!("expected queued messages, got {other:?}"),
    }
}

fn prompts(fixture: &mut Fixture, session: &SessionId) -> Vec<String> {
    fixture
        .transcript(session)
        .into_iter()
        .filter_map(|entry| match entry {
            TranscriptPayload::User { text } => Some(text),
            _ => None,
        })
        .collect()
}

/// Wait until the transcript holds `count` prompts.
fn wait_for_prompts(fixture: &mut Fixture, session: &SessionId, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let prompts = prompts(fixture, session);
        if prompts.len() >= count {
            return prompts;
        }
        assert!(
            Instant::now() < deadline,
            "only {prompts:?} ever reached the transcript"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn stopping_a_turn_pauses_its_queue_rather_than_throwing_it_away() {
    // opencodex keeps what was queued across a Stop; Ginka keeps it too, but
    // held rather than fired — the reader stopped the agent for a reason,
    // and the next prompt starting a moment later is not what "stop" meant.
    let mut fixture = Fixture::new();
    let session = busy_codex_with_two_queued(&mut fixture);
    fixture.ask(Request::CancelSession {
        session: session.clone(),
    });
    assert_eq!(fixture.settle(&session), SessionState::Cancelled);
    assert_eq!(
        queue_of(&mut fixture, &session),
        (vec!["second".to_string(), "third".to_string()], true)
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        prompts(&mut fixture, &session),
        vec!["first"],
        "nothing fired"
    );

    // Resuming dispatches the front of the queue into an idle session.
    fixture.ask(Request::SetQueuePaused {
        session: session.clone(),
        paused: false,
    });
    let prompts = wait_for_prompts(&mut fixture, &session, 2);
    assert_eq!(prompts[1], "second");
}

#[test]
fn interrupt_and_send_stops_the_turn_and_runs_that_prompt_next() {
    // opencodex's Steer: stop the current turn and send this one now.
    let mut fixture = Fixture::new();
    let session = busy_codex_with_two_queued(&mut fixture);
    let third = match fixture.ask(Request::QueuedMessages {
        session: session.clone(),
    }) {
        Response::QueuedMessages { messages, .. } => messages[1].id,
        other => panic!("expected queued messages, got {other:?}"),
    };
    fixture.ask(Request::InterruptWithQueuedMessage {
        session: session.clone(),
        id: third,
    });
    let prompts = wait_for_prompts(&mut fixture, &session, 2);
    assert_eq!(
        prompts[..2],
        ["first", "third"],
        "the chosen prompt jumps the queue"
    );
    let (waiting, paused) = queue_of(&mut fixture, &session);
    assert!(!paused, "an interrupt is not a stop: the queue keeps going");
    assert!(
        waiting == vec!["second".to_string()] || prompts.len() >= 3,
        "the rest waits its turn: {waiting:?}"
    );
}

#[test]
fn a_queued_message_can_be_chosen_over_steering_and_the_queue_cleared() {
    // Codex CLI's Tab: queue even where the transport could take it now.
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#,
            "#sleep 60000",
            r#"{"type":"result","subtype":"success","is_error":false}"#,
        ]
        .join("\n"),
        "take your time",
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while spoken(&fixture.transcript(&session)).is_empty() {
        assert!(Instant::now() < deadline, "the agent never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    fixture.ask(Request::QueueMessage {
        session: session.clone(),
        text: "later".into(),
    });
    assert_eq!(queue_of(&mut fixture, &session).0, vec!["later"]);
    assert_eq!(
        prompts(&mut fixture, &session),
        vec!["take your time"],
        "queued, not steered into the running turn"
    );
    fixture.ask(Request::ClearQueue {
        session: session.clone(),
    });
    assert!(queue_of(&mut fixture, &session).0.is_empty());
    fixture.ask(Request::CancelSession { session });
}

#[test]
fn a_queue_survives_a_daemon_restart_and_comes_back_held() {
    let mut fixture = Fixture::new();
    let session = busy_codex_with_two_queued(&mut fixture);
    fixture.ask(Request::CancelSession {
        session: session.clone(),
    });
    fixture.settle(&session);
    fixture.restart();
    assert_eq!(
        queue_of(&mut fixture, &session),
        (vec!["second".to_string(), "third".to_string()], true),
        "a restart must not fire prompts nobody is watching"
    );
    let listed = match fixture.ask(Request::ListWorkspaces { project: None }) {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert_eq!(
        listed
            .iter()
            .find(|summary| summary.id() == fixture.workspace)
            .map(|summary| summary.queued),
        Some(2),
        "the sidebar can say how much is waiting"
    );
}

#[test]
fn an_edited_prompt_runs_the_conversation_again_on_a_fresh_thread() {
    // MonoCode's edit-and-resend: the reader changes what they asked, and the
    // conversation goes on from there — not from the answer being replaced.
    let mut fixture = Fixture::new();
    let script = [
        r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[turn:{args}]"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-1"}"#,
    ]
    .join("\n");
    let session = fixture.start(&script, "first");
    fixture.settle(&session);
    fixture.ask(Request::SendMessage {
        session: session.clone(),
        text: "second".into(),
    });
    // The second turn's end, not just its prompt: a turn that has not begun
    // yet looks settled, and an edit is refused while one is running.
    fixture.wait_for(&session, |turns| turns.len() >= 2);
    fixture.settle(&session);

    let second = match fixture.ask(Request::SessionTranscript {
        session: session.clone(),
        after: None,
        limit: None,
    }) {
        Response::Transcript { entries } => entries
            .into_iter()
            .find(|entry| {
                matches!(&entry.payload, TranscriptPayload::User { text } if text == "second")
            })
            .expect("the second prompt is recorded")
            .seq,
        other => panic!("expected a transcript, got {other:?}"),
    };
    let edited = match fixture.ask(Request::EditPrompt {
        session: session.clone(),
        seq: second,
        text: "second, edited".into(),
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_ne!(edited.id, session, "the original conversation is kept");
    assert_eq!(
        edited.vendor_session_id, None,
        "not the thread with the old answer in it"
    );

    let prompts = wait_for_prompts(&mut fixture, &edited.id, 2);
    assert_eq!(prompts[..2], ["first", "second, edited"]);
    fixture.settle(&edited.id);
    assert!(
        !spoken(&fixture.transcript(&edited.id)).contains("resume vendor-1"),
        "the edited turn must not resume the replaced thread"
    );
    assert_eq!(
        self::prompts(&mut fixture, &session),
        vec!["first", "second"],
        "the original is untouched"
    );

    // A prompt that is not one is refused.
    assert!(
        fixture
            .service
            .handle(Request::EditPrompt {
                session: session.clone(),
                seq: second + 1,
                text: "no".into(),
            })
            .is_err()
    );
}

#[test]
fn an_acp_agent_is_prompted_once_it_has_opened_a_session_and_reloaded_after() {
    // ACP is a conversation: the prompt can only follow `session/new`'s
    // answer, which is what the driver's outbox is for.
    let mut fixture = Fixture::new();
    let session = fixture.start_with("gemini", "", "hello");
    let transcript = fixture.wait_for_said(&session, "[acp:acp-1:hello]");
    assert_eq!(fixture.settle(&session), SessionState::Finished);
    assert!(
        transcript.iter().any(|entry| matches!(
            entry,
            TranscriptPayload::Agent {
                event: AgentEvent::TurnEnd { turn: 1 }
            }
        )),
        "{transcript:?}"
    );
    assert_eq!(
        fixture.stored(&session).vendor_session_id.as_deref(),
        Some("acp-1")
    );

    fixture.ask(Request::SendMessage {
        session: session.clone(),
        text: "again".into(),
    });
    let transcript = fixture.wait_for_said(&session, "[acp:acp-1:again]");
    assert!(
        !spoken(&transcript).contains("[replayed]"),
        "a reloaded conversation is not recorded twice"
    );
}

#[test]
fn an_acp_permission_request_waits_for_the_reader_and_carries_the_choice_back() {
    let mut fixture = Fixture::new();
    let session = fixture.start_with("gemini", "", "needs permission");
    fixture.wait_for_state(&session, SessionState::AwaitingInput);
    let asked = fixture
        .transcript(&session)
        .into_iter()
        .find_map(|entry| match entry {
            TranscriptPayload::Agent {
                event: AgentEvent::AskUser { id, options, .. },
            } => Some((id, options)),
            _ => None,
        })
        .expect("the question is recorded");
    assert_eq!(asked.1, vec!["Allow", "Reject"]);
    fixture.ask(Request::RespondToAgent {
        session: session.clone(),
        request_id: asked.0,
        response: "Allow".into(),
    });
    fixture.wait_for_said(&session, "[permission:yes]");
}

#[test]
fn claude_asks_before_a_command_and_the_readers_choice_goes_back_on_its_input() {
    let mut fixture = Fixture::new();
    let script = [
        r#"{"type":"system","subtype":"init","session_id":"vendor-ask"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[args:{args}]"}]}}"#,
        r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test"}}}"#,
        "#read",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":{stdin_json}}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-ask"}"#,
    ]
    .join("\n");
    let session = fixture.start_with("claude", &script, "run the tests");
    fixture.wait_for_state(&session, SessionState::AwaitingInput);
    let transcript = fixture.transcript(&session);
    assert!(
        spoken(&transcript).contains("--permission-prompt-tool stdio"),
        "asked over the pipes rather than refused: {}",
        spoken(&transcript)
    );
    let (id, question, options) = transcript
        .into_iter()
        .find_map(|entry| match entry {
            TranscriptPayload::Agent {
                event:
                    AgentEvent::AskUser {
                        id,
                        question,
                        options,
                    },
            } => Some((id, question, options)),
            _ => None,
        })
        .expect("the ask is recorded as a card");
    assert!(question.contains("cargo test"), "{question}");
    assert_eq!(options, vec!["Allow", "Deny"]);

    fixture.ask(Request::RespondToAgent {
        session: session.clone(),
        request_id: id,
        response: "Allow".into(),
    });
    let transcript = fixture.wait_for_said(&session, r#""behavior":"allow""#);
    let said = spoken(&transcript);
    assert!(said.contains(r#""request_id":"req-1""#), "{said}");
    assert!(said.contains(r#""command":"cargo test""#), "{said}");
}

#[test]
fn codex_asks_before_a_command_over_its_app_server_and_carries_on_in_the_thread() {
    let mut fixture = Fixture::new();
    fixture.use_codex_app_server();
    let session = fixture.start_with("codex", "", "needs permission");
    fixture.wait_for_state(&session, SessionState::AwaitingInput);
    let (id, question, options) = fixture
        .transcript(&session)
        .into_iter()
        .find_map(|entry| match entry {
            TranscriptPayload::Agent {
                event:
                    AgentEvent::AskUser {
                        id,
                        question,
                        options,
                    },
            } => Some((id, question, options)),
            _ => None,
        })
        .expect("the approval is recorded as a card");
    assert!(question.contains("cargo test"), "{question}");
    assert_eq!(options, vec!["Allow", "Allow for this session", "Deny"]);

    fixture.ask(Request::RespondToAgent {
        session: session.clone(),
        request_id: id,
        response: "Allow for this session".into(),
    });
    fixture.wait_for_said(&session, "[decision:acceptForSession]");
    assert_eq!(
        fixture.stored(&session).vendor_session_id.as_deref(),
        Some("th-fake")
    );

    // The next turn continues the same thread.
    fixture.ask(Request::SendMessage {
        session: session.clone(),
        text: "again".into(),
    });
    fixture.wait_for_said(&session, "[codex:again]");
}

#[test]
fn a_chat_job_starts_a_conversation_and_is_skipped_while_it_is_working() {
    let mut fixture = Fixture::new();
    std::fs::write(
        &fixture.script,
        [
            r#"{"type":"system","subtype":"init","session_id":"vendor-cron"}"#,
            "#sleep 1500",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[cron:{prompt}]"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-cron"}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let job = match fixture.ask(Request::SaveCronJob {
        id: None,
        project: ginka_protocol::ProjectName("comet".into()),
        workspace: Some(fixture.workspace.clone()),
        name: "morning review".into(),
        schedule: "0 9 * * 1-5".into(),
        via: ginka_protocol::model::CronVia::Chat,
        agent: Some("claude".into()),
        body: "review yesterday's changes".into(),
        enabled: true,
    }) {
        Response::CronJob { job } => job,
        other => panic!("expected a job, got {other:?}"),
    };

    let started = match fixture.ask(Request::RunCronJob { id: job.id }) {
        Response::CronJob { job } => job.last_run.expect("it ran"),
        other => panic!("expected a job, got {other:?}"),
    };
    assert_eq!(started.outcome, ginka_protocol::model::CronOutcome::Started);
    let session = SessionId(started.detail.expect("the conversation it started"));

    // Still working: the next firing does not start a second one.
    match fixture.ask(Request::RunCronJob { id: job.id }) {
        Response::CronJob { job } => assert_eq!(
            job.last_run.map(|run| run.outcome),
            Some(ginka_protocol::model::CronOutcome::Skipped)
        ),
        other => panic!("expected a job, got {other:?}"),
    }

    let transcript = fixture.wait_for_said(&session, "[cron:review yesterday's changes]");
    assert!(transcript.iter().any(|entry| matches!(
        entry,
        TranscriptPayload::User { text } if text == "review yesterday's changes"
    )));
}

#[test]
fn a_long_transcript_is_read_from_its_end_a_page_at_a_time() {
    let mut fixture = Fixture::new();
    let session = fixture.start(&default_script_for_pages(), "first");
    fixture.settle(&session);
    let all = fixture.transcript_entries(&session);
    assert!(all.len() >= 4, "{all:?}");
    let last = all.last().unwrap().seq;

    let page = |fixture: &mut Fixture, before: Option<u64>, limit: u32| -> Vec<u64> {
        match fixture.ask(Request::SessionTranscriptTail {
            session: session.clone(),
            before,
            limit,
        }) {
            Response::Transcript { entries } => entries.iter().map(|entry| entry.seq).collect(),
            other => panic!("expected a transcript, got {other:?}"),
        }
    };
    assert_eq!(
        page(&mut fixture, None, 2),
        [last - 1, last],
        "the end, oldest first"
    );
    assert_eq!(
        page(&mut fixture, Some(last - 1), 2),
        [last - 3, last - 2],
        "the page before"
    );
    assert_eq!(
        page(&mut fixture, Some(2), 10),
        [1],
        "the start, however much was asked"
    );
    assert!(page(&mut fixture, Some(1), 10).is_empty());
}

fn default_script_for_pages() -> String {
    [
        r#"{"type":"system","subtype":"init","session_id":"vendor-pages"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one"}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-pages"}"#,
    ]
    .join("\n")
}

#[test]
fn a_ticket_an_agent_raised_starts_in_its_own_session_once() {
    let mut fixture = Fixture::new();
    let answer = [
        r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"vendor-1","usage":{"input_tokens":1,"output_tokens":1}}"#,
    ]
    .join("\n");
    let raiser = fixture.start(&answer, "fix the parser");
    fixture.settle(&raiser);

    let ticket = match fixture.ask(Request::RaiseTicket {
        workspace: None,
        from_session: Some(raiser.clone()),
        title: "Remove the dead retry helper".into(),
        summary: "Spotted while fixing the parser.".into(),
        prompt: "Delete retry() in src/net.rs and its tests.".into(),
    }) {
        Response::Ticket { ticket } => ticket,
        other => panic!("expected a ticket, got {other:?}"),
    };
    assert_eq!(
        ticket.workspace, fixture.workspace,
        "the raiser's workspace"
    );
    assert!(fixture.recorder.all().iter().any(|event| matches!(
        event,
        DaemonEvent::TicketsChanged { workspace } if workspace == &fixture.workspace
    )));
    match fixture.ask(Request::ListTickets {
        workspace: Some(fixture.workspace.clone()),
        all: false,
    }) {
        Response::Tickets { tickets } => assert_eq!(tickets, vec![ticket.clone()]),
        other => panic!("expected tickets, got {other:?}"),
    }

    let started = match fixture.ask(Request::StartTicket {
        ticket: ticket.id.clone(),
        agent: None,
        branch: None,
    }) {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    assert_ne!(started.id, raiser);
    assert_eq!(started.agent, "claude", "the raiser's agent picks it up");
    fixture.settle(&started.id);
    assert_eq!(
        fixture.transcript(&started.id).first(),
        Some(&TranscriptPayload::User {
            text: "Delete retry() in src/net.rs and its tests.".into()
        })
    );
    match fixture.ask(Request::ListTickets {
        workspace: None,
        all: true,
    }) {
        Response::Tickets { tickets } => {
            assert_eq!(tickets[0].session.as_ref(), Some(&started.id));
        }
        other => panic!("expected tickets, got {other:?}"),
    }
    let again = fixture
        .service
        .handle(Request::StartTicket {
            ticket: ticket.id.clone(),
            agent: None,
            branch: None,
        })
        .unwrap_err();
    assert!(
        again.message.contains("already started"),
        "{}",
        again.message
    );
}

#[test]
fn a_message_between_sessions_says_who_sent_it_and_how_to_answer() {
    let mut fixture = Fixture::new();
    let answer = [
        r#"{"type":"system","subtype":"init","session_id":"vendor-1"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"vendor-1","usage":{"input_tokens":1,"output_tokens":1}}"#,
    ]
    .join("\n");
    let sender = fixture.start(&answer, "review the plan");
    fixture.settle(&sender);
    let receiver = fixture.start(&answer, "write the code");
    fixture.settle(&receiver);

    fixture.ask(Request::MessageSession {
        from: sender.clone(),
        to: receiver.clone(),
        text: "Step 2 is missing a test.".into(),
    });
    fixture.settle(&receiver);
    let transcript = fixture.transcript(&receiver);
    let said = transcript
        .iter()
        .find_map(|entry| match entry {
            TranscriptPayload::User { text } if text.contains("Step 2") => Some(text.clone()),
            _ => None,
        })
        .expect("the message reached the receiver");
    assert!(
        said.contains(&format!("Message from Ginka session {sender}")),
        "{said}"
    );
    assert!(said.contains("ginka_session_send"), "{said}");

    let to_self = fixture
        .service
        .handle(Request::MessageSession {
            from: sender.clone(),
            to: sender.clone(),
            text: "hi".into(),
        })
        .unwrap_err();
    assert!(to_self.message.contains("itself"));
}
