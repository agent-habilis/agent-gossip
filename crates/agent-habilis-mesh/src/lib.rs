//! `agent-habilis-mesh` — the serverless gossip-network engine.
//!
//! This workspace-internal crate holds the transport-and-protocol engine
//! decoupled from any one application (its data model, its
//! CLI/MCP bindings, the library `api`). It is never published
//! (`publish = false`); a consumer crate depends on it and re-exports
//! the curated public surface.

// ── Role-based public facade ──────────────────────────────────────────────
//
// The modules below are grouped by what a consumer needs, not by the engine's
// internal topology: `protocol` (value types), `embed` (the seams you
// implement), `runtime` (start/stop a node), `ops` (what a hook may do), `net`
// (the quarantined iroh corner), `util` (host helpers). The implementation
// modules they re-export from are crate-private.
pub mod embed;
pub mod net;
pub mod ops;
pub mod runtime;

pub(crate) mod beacon;
pub(crate) mod blob;
pub(crate) mod daemon;
pub(crate) mod directory;
pub(crate) mod doc;

pub(crate) mod gossip;
// Creator-minted invites to an invite-only mesh. Engine-level: the redeem +
// decode primitives back `resolver::JoinTarget::Invite`, and `mint` is `pub`
// for the application layer's `invite` command.
pub(crate) mod invite;
pub(crate) mod lifecycle;
pub(crate) mod logging;
pub(crate) mod lookup;
pub mod protocol;
pub(crate) mod reassembly;
pub(crate) mod resolver;
pub(crate) mod transport;
pub mod util;

// Re-exported at the crate root so engine code (and the app's re-export) can
// reach the build version stamp as `crate::VERSION`.
pub use util::version::VERSION;

// The `NodeApp` / `NodeDriver` seams are `#[async_trait]`, so any consumer
// implementing them needs the same macro. Re-export it so a downstream crate
// annotates its impls with `#[agent_habilis_mesh::async_trait]` instead of
// taking its own (version-matched) `async-trait` dependency.
pub use async_trait::async_trait;

// Shared config for the crate's `proptest!` blocks. Overrides the default
// failure-persistence path so regression seeds land in
// `tests/proptest-regressions` rather than a `proptest-regressions` folder
// at the crate root. Every `proptest!` block opts in with
// `#![proptest_config(crate::proptest_support::config())]`. Kept last so it
// does not trip `clippy::items_after_test_module`.
#[cfg(test)]
pub(crate) mod proptest_support {
    use proptest::test_runner::{Config, FileFailurePersistence};

    pub(crate) fn config() -> Config {
        Config {
            failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
                "tests/proptest-regressions",
            ))),
            ..Config::default()
        }
    }
}
