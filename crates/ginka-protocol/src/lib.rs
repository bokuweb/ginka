//! Wire types shared by every Ginka process.
//!
//! This crate is deliberately dependency-light and free of any domain logic:
//! it is linked by the app, the daemon and the CLI alike, and it is the source
//! the TypeScript exporter reads. Anything that needs the filesystem, git or a
//! database belongs in `ginka-core`.
//!
//! The four layers, innermost first:
//!
//! - [`ids`] — the stable keys everything else is addressed by.
//! - [`model`] — the domain objects as they appear on the wire.
//! - [`event`] — the normalized agent stream and the daemon's pushes.
//! - [`handshake`] — how a client finds the daemon in the first place.
//! - [`rpc`] and [`envelope`] — the request surface and the frames carrying it.

pub mod envelope;
pub mod event;
pub mod handshake;
pub mod ids;
pub mod model;
pub mod rpc;

pub use envelope::{ClientMessage, RequestId, RpcError, Seq, ServerMessage};
pub use event::{AgentEvent, DaemonEvent, Usage};
pub use handshake::Handshake;
pub use ids::{CheckpointId, ProjectName, SessionId, WorkspaceId};
pub use model::{
    AgentStatus, BranchStatus, ChangeKind, ChangeSource, Changes, Checkpoint, DiffLine, FileChange,
    FileEntry, Hunk, LineKind, Project, ProjectKind, Session, SessionMatch, SessionState,
    TranscriptEntry, TranscriptPayload, UsageRow, UsageTotals, WorkspaceSummary, Worktree,
};
pub use rpc::{Request, Response};
