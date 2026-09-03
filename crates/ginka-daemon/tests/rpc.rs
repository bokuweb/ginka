//! The daemon over a real socket.
//!
//! `Service` is tested without a transport in `ginka-core`; what is left to
//! prove here is the transport itself — that the token is enforced, that a
//! mutation made by one client reaches another, and that a client which drops
//! its connection can pick the stream up where it left off.

use ginka_client::Client;
use ginka_core::{Paths, settings::DaemonSettings};
use ginka_daemon::Daemon;
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{Handshake, ProjectName};
use std::path::PathBuf;

/// A daemon listening on a loopback port, with its state in a temp directory.
struct Fixture {
    handshake: Handshake,
    paths: Paths,
    _home: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Fixture {
    fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(home.path().join("state"));
        let daemon = Daemon::bind(paths.clone(), DaemonSettings::default()).unwrap();
        let handshake = daemon.handshake();
        std::thread::spawn(move || {
            smol::block_on(daemon.serve()).expect("the daemon serves");
        });
        Self {
            handshake,
            paths,
            _home: home,
            work: tempfile::tempdir().unwrap(),
        }
    }

    async fn client(&self) -> Client {
        Client::connect(&self.handshake, None)
            .await
            .expect("the daemon accepts a client with the published token")
    }

    /// A repository with one commit, ready to register.
    fn repository(&self, name: &str) -> PathBuf {
        let root = self.work.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
        ] {
            if args[0] == "add" {
                std::fs::write(root.join("README.md"), "hello").unwrap();
            }
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
        }
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "first"])
            .output()
            .unwrap();
        assert!(output.status.success(), "git commit");
        root
    }
}

#[test]
fn a_client_with_the_published_token_is_served() {
    let fixture = Fixture::start();
    smol::block_on(async {
        let client = fixture.client().await;
        assert_eq!(client.request(Request::Ping).await.unwrap(), Response::Ack);
    });
}

#[test]
fn the_daemon_publishes_its_port_and_token_for_clients_to_find() {
    let fixture = Fixture::start();
    let published = ginka_daemon::handshake::read(&fixture.paths.daemon_handshake())
        .expect("a listening daemon publishes a handshake");
    assert_eq!(published.port, fixture.handshake.port);
    assert_eq!(published.token, fixture.handshake.token);
    assert_ne!(published.port, 0, "the published port is the one it got");
    assert!(
        published.token.len() >= 16,
        "the token must not be guessable"
    );
}

#[test]
fn a_second_daemon_does_not_take_over_a_live_one() {
    // Port 0 means the second daemon would get a different port and overwrite
    // the handshake file, orphaning the first — every client would then talk
    // to a daemon that owns none of the running agents.
    let fixture = Fixture::start();
    let error = match Daemon::bind(fixture.paths.clone(), DaemonSettings::default()) {
        Err(error) => error,
        Ok(_) => panic!("a daemon is already listening"),
    };
    assert!(error.to_string().contains("already running"), "{error}");
}

#[test]
fn a_stale_handshake_does_not_stop_a_daemon_from_starting() {
    // A daemon that was killed leaves its file behind; the port it names is
    // not answering, so the file is stale and must simply be replaced.
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(home.path().join("state"));
    paths.ensure().unwrap();
    ginka_daemon::handshake::write(
        &paths.daemon_handshake(),
        &Handshake {
            // Port 1 needs privileges to bind, so nothing is listening on it.
            port: 1,
            token: "stale".into(),
            pid: 999_999,
            version: "0.0.0".into(),
        },
    )
    .unwrap();

    let daemon = Daemon::bind(paths.clone(), DaemonSettings::default())
        .expect("a stale handshake is replaced");
    assert_ne!(daemon.handshake().token, "stale");
}

#[test]
fn a_client_with_the_wrong_token_is_refused() {
    let fixture = Fixture::start();
    let mut forged = fixture.handshake.clone();
    forged.token = "not-the-token".into();
    smol::block_on(async {
        let refused = Client::connect(&forged, None).await;
        assert!(refused.is_err(), "loopback is not authentication");
    });
}

#[test]
fn a_mutation_by_one_client_is_pushed_to_another() {
    let fixture = Fixture::start();
    let repository = fixture.repository("comet");
    smol::block_on(async {
        let watcher = fixture.client().await;
        let actor = fixture.client().await;

        actor
            .request(Request::AddProject { path: repository })
            .await
            .unwrap();

        // The watcher never asked for anything; it must learn anyway.
        let event = watcher.next_event().await.expect("an event arrives");
        assert_eq!(event.seq, 1);
        assert_eq!(event.payload, DaemonEvent::ProjectsChanged);

        let followed = watcher.next_event().await.unwrap();
        assert_eq!(
            followed.payload,
            DaemonEvent::WorkspacesChanged {
                project: ProjectName("comet".into())
            }
        );
    });
}

#[test]
fn a_reconnecting_client_is_given_the_events_it_missed() {
    let fixture = Fixture::start();
    let repository = fixture.repository("comet");
    smol::block_on(async {
        let watcher = fixture.client().await;
        let actor = fixture.client().await;
        actor
            .request(Request::AddProject {
                path: repository.clone(),
            })
            .await
            .unwrap();
        let first = watcher.next_event().await.unwrap();
        drop(watcher);

        // Something happens while nobody is watching.
        actor
            .request(Request::CreateWorkspace {
                project: ProjectName("comet".into()),
                branch: "harbor".into(),
                base: None,
            })
            .await
            .unwrap();

        let resumed = Client::connect(&fixture.handshake, Some(first.seq))
            .await
            .unwrap();
        let missed = resumed.next_event().await.expect("the gap is replayed");
        assert_eq!(
            missed.seq,
            first.seq + 1,
            "the stream picks up where it stopped"
        );
    });
}

#[test]
fn a_fresh_client_is_told_where_the_stream_is_rather_than_replaying_history() {
    let fixture = Fixture::start();
    let repository = fixture.repository("comet");
    smol::block_on(async {
        let actor = fixture.client().await;
        actor
            .request(Request::AddProject { path: repository })
            .await
            .unwrap();

        let latecomer = fixture.client().await;
        assert!(
            latecomer.current_seq() > 0,
            "the daemon says how far along it is"
        );
        // Nothing has happened since it connected, so nothing is waiting.
        assert!(latecomer.try_next_event().is_none());
    });
}

#[test]
fn a_watching_client_is_told_the_daemon_is_going_before_its_stream_ends() {
    // Otherwise a window has no way to tell "the daemon stopped" from "the
    // connection dropped", and reconnects into a daemon that is on its way out.
    let fixture = Fixture::start();
    smol::block_on(async {
        let watcher = fixture.client().await;
        let actor = fixture.client().await;
        actor.request(Request::Shutdown).await.unwrap();

        let announced = watcher.next_event().await.expect("an event arrives");
        assert_eq!(announced.payload, DaemonEvent::Shutdown);
    });
}

#[test]
fn a_failed_request_answers_with_an_error_and_leaves_the_connection_open() {
    let fixture = Fixture::start();
    smol::block_on(async {
        let client = fixture.client().await;
        let error = client
            .request(Request::ListWorkspaces {
                project: Some(ProjectName("absent".into())),
            })
            .await
            .expect_err("there is no such project");
        assert_eq!(error.code, "not_found");

        // The connection survives a rejected request.
        assert_eq!(client.request(Request::Ping).await.unwrap(), Response::Ack);
    });
}

#[test]
fn concurrent_requests_do_not_cross_their_replies() {
    // Ids correlate the answers; a client that pipelines must not have to
    // assume the daemon replies in order.
    let fixture = Fixture::start();
    let repository = fixture.repository("comet");
    smol::block_on(async {
        let client = fixture.client().await;
        client
            .request(Request::AddProject { path: repository })
            .await
            .unwrap();

        let projects = client.request(Request::ListProjects);
        let ping = client.request(Request::Ping);
        let (projects, ping) = futures_lite::future::zip(projects, ping).await;
        assert_eq!(ping.unwrap(), Response::Ack);
        match projects.unwrap() {
            Response::Projects { projects } => assert_eq!(projects.len(), 1),
            other => panic!("a ping's answer came back as {other:?}"),
        }
    });
}

#[test]
fn asking_the_daemon_to_shut_down_stops_it_and_clears_the_handshake() {
    let fixture = Fixture::start();
    let handshake_path = fixture.paths.daemon_handshake();
    smol::block_on(async {
        let client = fixture.client().await;
        // The answer, not a closed connection: a daemon that tears the socket
        // down before flushing hands `ginka daemon stop` a failure for having
        // succeeded.
        assert_eq!(
            client.request(Request::Shutdown).await.unwrap(),
            Response::Ack
        );

        // The daemon exits asynchronously; give it a moment to unwind.
        for _ in 0..100 {
            if !handshake_path.exists() {
                break;
            }
            smol::Timer::after(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !handshake_path.exists(),
            "a stopped daemon must not leave a file pointing at a dead port"
        );
    });
}
