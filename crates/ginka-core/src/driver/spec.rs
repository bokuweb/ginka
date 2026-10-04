//! What a session is started with.

use ginka_protocol::provider::SessionOptions;
use std::path::PathBuf;

/// Everything a driver needs to launch one session.
///
/// The binary is a path rather than a name because a provider's CLI may be
/// anywhere — a version manager, a nix profile, a checkout — and the user can
/// say where (`docs/roadmap.md` §3.3 N14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    /// The provider CLI to execute, already resolved from settings or `PATH`.
    pub binary: PathBuf,
    /// The worktree the agent works in. Every session is scoped to one.
    pub cwd: PathBuf,
    /// Model, effort, tier and access mode the session starts with.
    pub options: SessionOptions,
    /// The provider's own session id, when continuing an existing thread.
    pub resume: Option<String>,
}
