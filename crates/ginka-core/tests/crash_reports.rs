//! A panic leaves a report in the logs directory.

const FAKE_AGENT: &str = env!("CARGO_BIN_EXE_ginka-fake-agent");

#[test]
fn a_panic_leaves_a_report_beside_the_logs() {
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(FAKE_AGENT)
        .arg("--crash")
        .env("GINKA_HOME", home.path())
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    assert!(!output.status.success(), "it crashed");
    let reports: Vec<_> = std::fs::read_dir(home.path().join("logs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("crash-fake-"))
        })
        .collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let text = std::fs::read_to_string(&reports[0]).unwrap();
    assert!(text.contains("the fake agent was asked to crash"), "{text}");
    assert!(text.contains("fake_agent.rs"), "where it happened: {text}");
    // The default hook still ran: the reader who launched it sees it too.
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("asked to crash"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
