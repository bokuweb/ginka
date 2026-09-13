//! Integration coverage for the stdio language-server transport.

use ginka_core::lsp::{LanguageServer, LanguageServerLaunch};
use lsp_types::{HoverContents, MarkedString, Position};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[test]
fn language_server_round_trip_tracks_the_latest_buffer() {
    let temp = tempfile::tempdir().expect("temp directory");
    let document = temp.path().join("main.rs");
    std::fs::write(&document, "fn before() {}\n").expect("fixture is writable");
    let launch = LanguageServerLaunch {
        program: PathBuf::from(env!("CARGO_BIN_EXE_ginka-fake-agent")),
        arguments: vec!["--lsp".to_string()],
        root: temp.path().to_path_buf(),
        document,
        language_id: "rust".to_string(),
    };

    let server = LanguageServer::start(&launch, "fn before() {}\n").expect("server starts");
    server
        .change(1, "fn after() {}\n".to_string())
        .expect("change is delivered");

    let hover = server
        .hover(Position::new(0, 3))
        .expect("hover response")
        .expect("hover exists");
    assert_eq!(
        hover.contents,
        HoverContents::Scalar(MarkedString::String("fn after() {}\n".to_string()))
    );

    let definitions = server
        .definitions(Position::new(0, 3))
        .expect("definition response");
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].target_uri, *server.document_uri());

    let deadline = Instant::now() + Duration::from_secs(1);
    let diagnostics = loop {
        if let Some(diagnostics) = server.take_diagnostics() {
            break diagnostics;
        }
        assert!(Instant::now() < deadline, "diagnostics were not published");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "fake diagnostic");
}
