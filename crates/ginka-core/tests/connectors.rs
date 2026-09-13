//! What a chat connector asks of the service, exercised without one.
//!
//! The Slack runner is a client of the same `Service` the CLI reaches; what
//! is pinned here is the protocol surface it depends on — a session that
//! knows the thread it came from, an access mode that reaches the agent, and
//! the connector requests — against the fake agent rather than Slack.

mod support;

use ginka_core::connector::{ConnectorControl, ConnectorsSettings};
use ginka_core::driver::{Registry, claude::ClaudeDriver};
use ginka_core::service::{EventSink, Service};
use ginka_core::{Paths, db, settings};
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::model::{ConnectorState, SessionOrigin, SessionState, TranscriptPayload};
use ginka_protocol::provider::AccessMode;
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{AgentEvent, ProjectName, SessionId, WorkspaceId};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FAKE_AGENT: &str = env!("CARGO_BIN_EXE_ginka-fake-agent");

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<DaemonEvent>>,
}

impl EventSink for Recorder {
    fn emit(&self, event: DaemonEvent) {
        self.events.lock().unwrap().push(event);
    }
}

struct Fixture {
    service: Service,
    recorder: Arc<Recorder>,
    paths: Paths,
    workspace: WorkspaceId,
    script: std::path::PathBuf,
    _home: tempfile::TempDir,
    _work: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(home.path().join("state"));
        paths.ensure().unwrap();
        let script = work.path().join("agent-script.txt");
        let recorder = Arc::new(Recorder::default());
        let mut drivers = Registry::with_defaults();
        drivers.insert(Arc::new(
            ClaudeDriver::with_program(FAKE_AGENT)
                .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
        ));
        let mut service = Service::new(
            paths.clone(),
            db::open_in_memory().unwrap(),
            recorder.clone(),
        )
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
            recorder,
            paths,
            workspace,
            script,
            _home: home,
            _work: work,
        }
    }

    fn ask(&mut self, request: Request) -> Response {
        self.service
            .handle(request)
            .unwrap_or_else(|error| panic!("request failed: {error}"))
    }

    fn start(&mut self, script: &str, prompt: &str, origin: Option<SessionOrigin>) -> SessionId {
        std::fs::write(&self.script, script).unwrap();
        match self.ask(Request::StartSession {
            workspace: self.workspace.clone(),
            agent: "claude".into(),
            prompt: prompt.into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            account: None,
            access_mode: Some(AccessMode::Auto),
            origin,
        }) {
            Response::Session { session } => session.id,
            other => panic!("expected a session, got {other:?}"),
        }
    }

    fn settle(&mut self, id: &SessionId) -> SessionState {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = match self.ask(Request::ListSessions {
                workspace: None,
                origin: None,
            }) {
                Response::Sessions { sessions } => {
                    sessions
                        .into_iter()
                        .find(|session| &session.id == id)
                        .expect("stored")
                        .state
                }
                other => panic!("expected sessions, got {other:?}"),
            };
            if !matches!(
                state,
                SessionState::Starting | SessionState::Running | SessionState::AwaitingInput
            ) {
                return state;
            }
            assert!(Instant::now() < deadline, "never left {state:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn agent_text(&mut self, id: &SessionId) -> String {
        match self.ask(Request::SessionTranscript {
            session: id.clone(),
            after: None,
            limit: None,
        }) {
            Response::Transcript { entries } => entries
                .into_iter()
                .filter_map(|entry| match entry.payload {
                    TranscriptPayload::Agent {
                        event: AgentEvent::TextDelta { text },
                    } => Some(text),
                    _ => None,
                })
                .collect(),
            other => panic!("expected a transcript, got {other:?}"),
        }
    }
}

fn origin(thread: &str) -> SessionOrigin {
    SessionOrigin {
        connector: "slack".into(),
        channel: "C1".into(),
        thread: thread.into(),
    }
}

/// Answers with the arguments it was started with, so a test can see what
/// reached the agent.
const ECHO_ARGS: &str = concat!(
    r#"{"type":"system","subtype":"init","session_id":"{session}","model":"fake"}"#,
    "\n",
    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"args: {args}"}]},"session_id":"{session}"}"#,
    "\n",
    r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"{session}","usage":{"input_tokens":1,"output_tokens":1}}"#,
);

#[test]
fn a_thread_finds_its_session_and_cannot_have_two() {
    let mut fixture = Fixture::new();
    let first = fixture.start(ECHO_ARGS, "fix it", Some(origin("1.0")));
    fixture.settle(&first);

    let found = match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: Some(origin("1.0")),
    }) {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, first);
    assert_eq!(found[0].origin, Some(origin("1.0")));
    assert_eq!(found[0].access_mode, AccessMode::Auto);

    // A second start on the same thread is refused, naming the first.
    let refused = fixture
        .service
        .handle(Request::StartSession {
            workspace: fixture.workspace.clone(),
            agent: "claude".into(),
            prompt: "again".into(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            account: None,
            access_mode: None,
            origin: Some(origin("1.0")),
        })
        .unwrap_err();
    assert!(refused.message.contains(&first.0), "{}", refused.message);

    // Told to start fresh, the thread can be answered by a new session, and
    // the old one still says where it came from.
    fixture.ask(Request::CloseSessionOrigin {
        session: first.clone(),
    });
    let second = fixture.start(ECHO_ARGS, "again", Some(origin("1.0")));
    fixture.settle(&second);
    let listed = match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: Some(origin("1.0")),
    }) {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert_eq!(
        listed.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![second]
    );
    let all = match fixture.ask(Request::ListSessions {
        workspace: None,
        origin: None,
    }) {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    let old = all.iter().find(|s| s.id == first).expect("still stored");
    assert_eq!(old.origin, Some(origin("1.0")));

    // A session that never came from a thread has no origin to close.
    let plain = fixture.start(ECHO_ARGS, "plain", None);
    fixture.settle(&plain);
    assert_eq!(
        fixture
            .service
            .handle(Request::CloseSessionOrigin { session: plain })
            .unwrap_err()
            .code,
        "failed"
    );
}

#[test]
fn the_access_mode_reaches_the_agent_on_every_turn() {
    let mut fixture = Fixture::new();
    let session = fixture.start(ECHO_ARGS, "first", None);
    fixture.settle(&session);
    let first_turn = fixture.agent_text(&session);
    assert!(
        first_turn.contains("--permission-mode bypassPermissions"),
        "the binding's ceiling is a launch argument: {first_turn}"
    );

    // A follow-up runs at the mode the session started with, not the
    // default, because widening or narrowing silently is what N2 forbids.
    fixture.ask(Request::SendMessage {
        session: session.clone(),
        text: "second".into(),
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = fixture.agent_text(&session);
        if text.matches("--permission-mode bypassPermissions").count() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second turn never ran: {text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A connector that records what the service asked of it.
struct Scripted {
    reloads: Mutex<Vec<ConnectorsSettings>>,
    tested: Mutex<Vec<String>>,
}

impl ConnectorControl for Scripted {
    fn id(&self) -> &'static str {
        "slack"
    }
    fn state(&self) -> ConnectorState {
        ConnectorState {
            id: "slack".into(),
            enabled: true,
            connected: true,
            since: Some(1),
            last_error: None,
            bindings: Vec::new(),
        }
    }
    fn reload(&self, settings: &ConnectorsSettings) {
        self.reloads.lock().unwrap().push(settings.clone());
    }
    fn test(&self, channel: &str) -> anyhow::Result<()> {
        self.tested.lock().unwrap().push(channel.to_string());
        Ok(())
    }
}

#[test]
fn a_connector_nobody_registered_still_says_why_it_is_not_running() {
    let mut fixture = Fixture::new();
    let states = match fixture.ask(Request::ListConnectors) {
        Response::Connectors { connectors } => connectors,
        other => panic!("expected connectors, got {other:?}"),
    };
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].id, "slack");
    assert!(!states[0].enabled);
    assert!(!states[0].connected);
    assert!(
        states[0]
            .last_error
            .as_deref()
            .is_some_and(|why| why.contains("not configured")),
        "{states:?}"
    );
    let error = fixture
        .service
        .handle(Request::TestConnector {
            connector: "slack".into(),
            channel: "C1".into(),
        })
        .unwrap_err();
    assert_eq!(error.code, "not_found");
}

#[test]
fn allowing_a_sender_is_written_down_and_the_running_connector_is_told() {
    let mut fixture = Fixture::new();
    let scripted = Arc::new(Scripted {
        reloads: Mutex::new(Vec::new()),
        tested: Mutex::new(Vec::new()),
    });
    fixture.service.register_connector(scripted.clone());

    fixture.ask(Request::AllowConnectorSender {
        connector: "slack".into(),
        sender: " U01ABC2DEF3 ".into(),
    });
    // Twice is once: the allowlist is a set.
    fixture.ask(Request::AllowConnectorSender {
        connector: "slack".into(),
        sender: "U01ABC2DEF3".into(),
    });

    let written: settings::DaemonSettings = settings::load(&fixture.paths.daemon_settings());
    let slack = written
        .connectors
        .slack
        .expect("the slack settings were created");
    assert_eq!(slack.allowed_users, vec!["U01ABC2DEF3".to_string()]);

    let reloads = scripted.reloads.lock().unwrap();
    assert_eq!(reloads.len(), 2);
    assert_eq!(
        reloads[1].slack.as_ref().unwrap().allowed_users,
        vec!["U01ABC2DEF3".to_string()]
    );
    assert!(
        fixture
            .recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, DaemonEvent::ConnectorStateChanged { .. })),
        "every window is told the connector's state moved"
    );

    // A registered connector answers for itself.
    let states = match fixture.ask(Request::ListConnectors) {
        Response::Connectors { connectors } => connectors,
        other => panic!("expected connectors, got {other:?}"),
    };
    assert_eq!(states.len(), 1);
    assert!(states[0].connected);
    fixture.ask(Request::TestConnector {
        connector: "slack".into(),
        channel: "C9".into(),
    });
    assert_eq!(*scripted.tested.lock().unwrap(), vec!["C9".to_string()]);

    // A connector this build does not have is not found, and an empty
    // sender is refused rather than written.
    assert_eq!(
        fixture
            .service
            .handle(Request::AllowConnectorSender {
                connector: "discord".into(),
                sender: "U1".into(),
            })
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        fixture
            .service
            .handle(Request::AllowConnectorSender {
                connector: "slack".into(),
                sender: "  ".into(),
            })
            .unwrap_err()
            .code,
        "failed"
    );
}
