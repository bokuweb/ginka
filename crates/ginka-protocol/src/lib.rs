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
pub mod provider;
pub mod rpc;
pub mod session;

pub use envelope::{
    ClientMessage, HandshakeRejection, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, RequestId,
    RpcError, Seq, ServerMessage,
};
pub use event::{
    AgentEvent, ContextUsage, DaemonEvent, SubagentStep, SubagentStepKind, SubagentStepStatus,
    TaskItem, TaskStatus, Usage,
};
pub use handshake::Handshake;
pub use ids::{AccountId, CheckpointId, ProjectName, SessionId, TerminalId, WorkspaceId};
pub use model::{
    Account, AgentStatus, BranchStatus, ChangeKind, ChangeSource, Changes, Checkpoint, CliSession,
    CommandScope, DiffLine, DiffSide, FileChange, FileEntry, Hunk, LineKind, LoginCommand,
    PlanSnapshot, PlanSource, PlanUsage, PlanWindow, Project, ProjectKind, PullRequest,
    PullRequestState, ReviewComment, Session, SessionMatch, SessionState, SlashCommand,
    TranscriptEntry, TranscriptPayload, UsageRow, UsageTotals, WorkspaceSummary, Worktree,
};
pub use provider::{AccessMode, OptionOutcome, ProviderKind, ProviderModel, SessionOptions};
pub use rpc::{Request, Response};
pub use session::SessionTitle;
