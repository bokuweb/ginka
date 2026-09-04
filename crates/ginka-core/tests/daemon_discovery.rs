//! How the app and the CLI find a running daemon, and how the daemon
//! publishes itself.

use ginka_core::Paths;
use ginka_core::daemon;
use ginka_protocol::envelope::PROTOCOL_VERSION;

#[test]
fn publishing_makes_the_daemon_discoverable() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());

    let published = daemon::publish(&paths, 8123).unwrap();
    assert_eq!(published.port, 8123);
    assert_eq!(published.protocol_version, PROTOCOL_VERSION);
    assert_eq!(published.pid, std::process::id());

    let found = daemon::read(&paths).unwrap().unwrap();
    assert_eq!(found, published);
}

#[test]
fn each_run_gets_its_own_token_and_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());

    let first = daemon::publish(&paths, 1).unwrap();
    let second = daemon::publish(&paths, 1).unwrap();

    assert_ne!(
        first.token, second.token,
        "a token is per run, not per user"
    );
    assert_ne!(first.epoch, second.epoch, "the epoch identifies the run");
    assert!(first.token.len() >= 32, "a guessable token is no token");
}

#[test]
fn the_handshake_file_is_not_readable_by_other_users() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());
    daemon::publish(&paths, 1).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(paths.daemon_handshake())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the token is in this file: {mode:o}");
    }
}

#[test]
fn publishing_leaves_no_partial_file_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());
    daemon::publish(&paths, 1).unwrap();

    let strays: Vec<String> = std::fs::read_dir(paths.root())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("daemon") && !name.ends_with(".json"))
        .collect();
    assert!(strays.is_empty(), "{strays:?}");
}

#[test]
fn no_daemon_is_a_miss_rather_than_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());
    assert!(daemon::read(&paths).unwrap().is_none());
}

#[test]
fn a_corrupt_handshake_file_reads_as_no_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.daemon_handshake(), "{ half written").unwrap();

    // A daemon that died mid-write must not stop the next one from starting.
    assert!(daemon::read(&paths).unwrap().is_none());
}

#[test]
fn withdrawing_removes_the_advertisement() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::with_root(tmp.path());
    daemon::publish(&paths, 1).unwrap();
    daemon::withdraw(&paths).unwrap();

    assert!(daemon::read(&paths).unwrap().is_none());
    // Withdrawing when nothing is published is not an error either.
    daemon::withdraw(&paths).unwrap();
}

#[test]
fn a_token_is_compared_in_constant_time_and_rejects_the_wrong_one() {
    assert!(daemon::token_matches("abcdef", "abcdef"));
    assert!(!daemon::token_matches("abcdef", "abcdeg"));
    assert!(!daemon::token_matches("abcdef", "abcde"));
    assert!(
        !daemon::token_matches("", ""),
        "an empty token is never valid"
    );
}
