//! Starting the daemon on demand.
//!
//! Neither the app nor the CLI should make the user run a background service
//! by hand: whichever notices there is no daemon starts one. These tests use
//! the real binary, with `GINKA_HOME` pointed at a temporary directory so a
//! spawned daemon cannot touch the developer's state.

use ginka_client::Discovery;
use ginka_core::Paths;
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{Handshake, ProjectName};

/// Where the daemon binary this test built lives.
const DAEMON: &str = env!("CARGO_BIN_EXE_ginka-daemon");

struct Home {
    paths: Paths,
    _dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("state"));
        paths.ensure().unwrap();
        Self { paths, _dir: dir }
    }

    fn discovery(&self) -> Discovery {
        Discovery::new(self.paths.daemon_handshake())
            .with_binary(DAEMON)
            .with_home(self.paths.root())
    }
}

/// Stop whatever daemon a test started, so it does not outlive the run.
fn stop(discovery: &Discovery) {
    smol::block_on(async {
        if let Ok(client) = discovery.connect(None).await {
            client.request(Request::Shutdown).await.ok();
        }
    });
}

#[test]
fn a_client_starts_a_daemon_when_none_is_running() {
    let home = Home::new();
    let discovery = home.discovery();
    assert!(discovery.published().is_none(), "nothing is running yet");

    smol::block_on(async {
        let client = discovery
            .connect(None)
            .await
            .expect("a client with no daemon starts one");
        assert_eq!(client.request(Request::Ping).await.unwrap(), Response::Ack);
    });
    assert!(
        discovery.published().is_some(),
        "the daemon it started published itself"
    );
    stop(&discovery);
}

#[test]
fn a_second_client_reuses_the_running_daemon() {
    let home = Home::new();
    let discovery = home.discovery();
    smol::block_on(async {
        let first = discovery.connect(None).await.unwrap();
        let published = discovery.published().expect("a daemon is running");

        let second = discovery.connect(None).await.unwrap();
        assert_eq!(
            discovery.published().unwrap().pid,
            published.pid,
            "the second client must not start a rival daemon"
        );

        // Both are talking to the same state.
        first.request(Request::ListProjects).await.unwrap();
        second.request(Request::ListProjects).await.unwrap();
    });
    stop(&discovery);
}

#[test]
fn a_stale_handshake_is_replaced_rather_than_trusted() {
    let home = Home::new();
    ginka_daemon::handshake::write(
        &home.paths.daemon_handshake(),
        &Handshake {
            protocol_version: ginka_protocol::envelope::PROTOCOL_VERSION,
            // Binding port 1 needs privileges, so nothing is listening there.
            port: 1,
            token: "stale".into(),
            pid: 999_999,
            version: "0.0.0".into(),
            epoch: 1,
        },
    )
    .unwrap();

    let discovery = home.discovery();
    smol::block_on(async {
        let client = discovery
            .connect(None)
            .await
            .expect("a dead daemon's file must not strand the client");
        assert_eq!(client.request(Request::Ping).await.unwrap(), Response::Ack);
    });
    assert_ne!(discovery.published().unwrap().token, "stale");
    stop(&discovery);
}

#[test]
fn a_daemon_from_an_older_build_is_replaced_rather_than_waited_for() {
    // The lock keeps a second daemon out, so an upgrade has to retire the
    // old one or no client of the new build could ever connect.
    let home = Home::new();
    let discovery = home.discovery();
    smol::block_on(async {
        discovery
            .connect(None)
            .await
            .expect("the first daemon starts");
    });
    let mut old = discovery.published().expect("it published itself");
    // As a daemon from an older build looks to this client: an older
    // protocol in its file, and a refusal when it is spoken to.
    old.protocol_version -= 1;
    old.token = "refused".into();
    ginka_daemon::handshake::write(&home.paths.daemon_handshake(), &old).unwrap();

    smol::block_on(async {
        let client = discovery
            .connect(None)
            .await
            .expect("an older daemon must not strand a newer client");
        assert_eq!(client.request(Request::Ping).await.unwrap(), Response::Ack);
    });
    let now = discovery.published().unwrap();
    assert_ne!(now.pid, old.pid, "the old daemon was replaced");
    assert_eq!(
        now.protocol_version,
        ginka_protocol::envelope::PROTOCOL_VERSION
    );
    stop(&discovery);
}

#[test]
fn a_spawned_daemon_keeps_the_state_it_was_given() {
    // The daemon inherits `GINKA_HOME`, so what a client writes through it
    // lands in the directory the client meant, not in the user's real one.
    let home = Home::new();
    let discovery = home.discovery();
    let work = tempfile::tempdir().unwrap();
    let repository = work.path().join("comet");
    std::fs::create_dir_all(&repository).unwrap();
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "T"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(repository.join("README.md"), "hello").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-m", "first"]] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(&args)
            .output()
            .unwrap();
    }

    smol::block_on(async {
        let client = discovery.connect(None).await.unwrap();
        client
            .request(Request::AddProject {
                path: repository.clone(),
                label: None,
            })
            .await
            .unwrap();
        match client.request(Request::ListProjects).await.unwrap() {
            Response::Projects { projects } => {
                assert_eq!(projects[0].name, ProjectName("comet".into()))
            }
            other => panic!("expected projects, got {other:?}"),
        }
    });
    assert!(
        home.paths.database().is_file(),
        "the daemon wrote into the home it was given"
    );
    stop(&discovery);
}

#[test]
fn a_daemon_asked_to_stop_by_signal_leaves_nothing_behind() {
    // `pkill ginka-daemon`, a logout, a machine shutting down: the daemon has
    // to go, and it must not leave a file pointing at a port nothing answers.
    let home = Home::new();
    let discovery = home.discovery();
    smol::block_on(async {
        discovery.connect(None).await.expect("a daemon starts");
    });
    let published = discovery.published().expect("it published itself");

    let killed = std::process::Command::new("kill")
        .arg(published.pid.to_string())
        .status()
        .expect("kill is on PATH");
    assert!(killed.success());

    for _ in 0..200 {
        if discovery.published().is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    // Leave nothing running for the next test either.
    stop(&discovery);
    panic!("the daemon ignored the signal, or left its handshake behind");
}

#[test]
fn a_second_daemon_on_the_same_state_leaves_the_first_alone() {
    let home = Home::new();
    let discovery = home.discovery();
    smol::block_on(async {
        discovery
            .connect(None)
            .await
            .expect("the first daemon starts");
    });
    let first = discovery.published().expect("it published itself").pid;

    let status = std::process::Command::new(DAEMON)
        .env("GINKA_HOME", home.paths.root())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the second one leaves quietly");
    assert_eq!(
        discovery.published().map(|handshake| handshake.pid),
        Some(first),
        "and does not publish over the first"
    );
    stop(&discovery);
}

#[test]
fn clients_arriving_together_share_one_daemon() {
    let home = Home::new();
    let discovery = home.discovery();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let discovery = discovery.clone();
            std::thread::spawn(move || {
                smol::block_on(async {
                    let client = discovery.connect(None).await.expect("connects");
                    client.request(Request::Ping).await.unwrap();
                })
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    // However many clients raced to start one, only one daemon got past the
    // lock to serve, and its log says so once.
    let ready = std::fs::read_dir(home.paths.logs())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("daemon"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .map(|text| text.matches("daemon ready").count())
        .sum::<usize>();
    assert_eq!(ready, 1, "one daemon served every client");
    stop(&discovery);
}
