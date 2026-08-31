//! Wire types shared by every Ginka process.
//!
//! This crate is deliberately dependency-light and free of any domain logic:
//! it is linked by the app, the daemon and the CLI alike, and it is the source
//! the TypeScript exporter reads. Anything that needs the filesystem, git or a
//! database belongs in `ginka-core`.

pub mod envelope;
pub mod ids;

pub use envelope::{ClientMessage, RpcError, ServerMessage};
pub use ids::{ProjectName, WorkspaceId};
