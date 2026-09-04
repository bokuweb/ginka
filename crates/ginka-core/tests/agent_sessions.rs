//! Running an agent, end to end, against the fake agent binary.
//!
//! Nothing here talks to a vendor: the scripted stand-in speaks Claude Code's
//! `stream-json`, so the real driver parses it and only the process on the far
//! end is fake. That is what makes supervision, cancellation, queued follow-ups
//! and resume testable without a network or a token budget.

mod support;

use ginka_core::driver::{Registry, claude::ClaudeDriver};
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

struct Fixture {
    service: Service,
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
        let mut drivers = Registry::with_defaults();
        drivers.insert(Arc::new(
            ClaudeDriver::with_program(FAKE_AGENT)
                .with_env("GINKA_FAKE_AGENT_SCRIPT", script.to_string_lossy()),
        ));
        let mut service = Service::new(paths, db::open_in_memory().unwrap(), recorder.clone())
            .with_drivers(drivers);

        let root = work.path().join("comet");
        support::repository(&root);
        service.handle(Request::AddProject { path: root }).unwrap();
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
            workspace,
            script,
            _home: home,
            _work: work,
        }
    }

    /// Point the agent at `script`, and start a session with `prompt`.
    fn start(&mut self, script: &str, prompt: &str) -> SessionId {
        std::fs::write(&self.script, script).unwrap();
        match self
            .service
            .handle(Request::StartSession {
                workspace: self.workspace.clone(),
                agent: "claude".into(),
                prompt: prompt.into(),
                model: None,
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
            .handle(Request::ListSessions { workspace: None })
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

    /// The stored session, as the daemon has it.
    fn stored(&mut self, id: &SessionId) -> ginka_protocol::model::Session {
        match self
            .service
            .handle(Request::ListSessions { workspace: None })
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
                event: AgentEvent::TurnEnd { turn: 1 }
            }
        )),
        "the turn boundary is recorded: {transcript:?}"
    );
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
        .handle(Request::ListSessions { workspace: None })
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
        .handle(Request::ListSessions { workspace: None })
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
    let mut fixture = Fixture::new();
    let session = fixture.start(
        &[
            r#"{"type":"system","subtype":"init","session_id":"vendor-9"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"[turn:{args}]"}]}}"#,
            "#sleep 400",
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"vendor-9"}"#,
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
                spoken.contains("--resume vendor-9"),
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
    match fixture.ask(Request::ListSessions { workspace: None }) {
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
        Response::Usage { by_day, by_agent } => {
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
                },
                ginka_protocol::rpc::Attempt {
                    agent: "claude".into(),
                    model: None,
                },
                // A driver this build does not have: the arm fails and the
                // others carry on.
                ginka_protocol::rpc::Attempt {
                    agent: "no-such-agent".into(),
                    model: None,
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
