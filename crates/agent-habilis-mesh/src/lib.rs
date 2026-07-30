//! `agent-habilis-mesh` — the serverless gossip-network engine.
//!
//! This workspace-internal crate holds the transport-and-protocol engine
//! decoupled from any one application (its data model, its
//! CLI/MCP bindings, the library `api`). It is never published
//! (`publish = false`); the `agent-gossip` crate depends on it and re-exports
//! the curated public surface.

// Re-exported so the app crate can name the multi-hop transport's public types
// (e.g. `LinkVector`) without a second direct dependency.
pub use iroh_multihop_transport;

pub(crate) mod beacon;
pub mod blob;
pub mod daemon;
pub mod directory;
pub mod doc;

pub mod gossip;
// Creator-minted invites to an invite-only mesh. Engine-level: the redeem +
// decode primitives back `resolver::JoinTarget::Invite`, and `mint` is `pub`
// for the application layer's `invite` command.
pub mod invite;
pub mod lifecycle;
pub mod logging;
pub mod lookup;
pub mod protocol;
pub mod reassembly;
pub mod resolver;
pub mod transport;
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
