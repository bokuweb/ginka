//! Write the wire protocol out as TypeScript.
//!
//! The Rust definitions are the source of truth for every process in the
//! system, and for anything that is written against the daemon later — a web
//! client, a mobile client, an editor extension. Generating their types from
//! here is what keeps a rename from becoming a silent incompatibility.
//!
//! Run it with `cargo run -p ginka-protocol --features export --bin
//! export-types`. The output is not committed: it is a build product, and a
//! checked-in copy is one more thing that can be out of date.

use ginka_protocol::envelope::{ClientMessage, RpcError, ServerMessage};
use ginka_protocol::event::{AgentEvent, DaemonEvent, Usage};
use ginka_protocol::handshake::Handshake;
use ginka_protocol::ids::{CheckpointId, ProjectName, SessionId, WorkspaceId};
use ginka_protocol::model::{
    BranchStatus, Checkpoint, Project, ProjectKind, Session, SessionState, TranscriptEntry,
    TranscriptPayload, WorkspaceSummary, Worktree,
};
use ginka_protocol::rpc::{Request, Response};
use ts_rs::{Config, TS};

fn main() {
    // Written relative to the working directory, so `cargo run` from the
    // workspace root puts them where a client would look for them.
    let directory = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bindings".to_string());
    // `number`, not `bigint`: these types describe what `JSON.parse` produces
    // from the daemon's frames, and serde writes a `u64` as a JSON number.
    let config = Config::new()
        .with_out_dir(&directory)
        .with_large_int("number");
    // Listed rather than discovered: a type that is not reachable from the
    // envelope is not part of the protocol, and this is where that is decided.
    let exported: Vec<Result<(), ts_rs::ExportError>> = vec![
        ProjectName::export_all(&config),
        WorkspaceId::export_all(&config),
        SessionId::export_all(&config),
        CheckpointId::export_all(&config),
        Project::export_all(&config),
        ProjectKind::export_all(&config),
        Worktree::export_all(&config),
        BranchStatus::export_all(&config),
        Session::export_all(&config),
        SessionState::export_all(&config),
        TranscriptEntry::export_all(&config),
        TranscriptPayload::export_all(&config),
        Checkpoint::export_all(&config),
        WorkspaceSummary::export_all(&config),
        AgentEvent::export_all(&config),
        Usage::export_all(&config),
        DaemonEvent::export_all(&config),
        Request::export_all(&config),
        Response::export_all(&config),
        ClientMessage::export_all(&config),
        ServerMessage::export_all(&config),
        RpcError::export_all(&config),
        Handshake::export_all(&config),
    ];

    let mut failures = 0;
    for result in exported {
        if let Err(error) = result {
            eprintln!("export failed: {error}");
            failures += 1;
        }
    }
    if failures > 0 {
        std::process::exit(1);
    }
    println!("exported {} protocol types into {directory}", 23 - failures);
}
