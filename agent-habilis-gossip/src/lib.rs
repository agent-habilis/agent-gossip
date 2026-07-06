//! `agent-habilis-gossip` — the serverless gossip-network engine.
//!
//! This workspace-internal crate holds the transport-and-protocol engine
//! decoupled from the `agent-gossip` application (the A2A data model, the
//! CLI/MCP bindings, the `embed` facade). It is never published
//! (`publish = false`); the `agent-gossip` crate depends on it and re-exports
//! the curated public surface.

pub mod beacon;
pub mod blob;
pub mod daemon;
pub mod directory;
pub mod embed;
pub mod gossip;
// Creator-minted invites to an invite-only swarm. Engine-level: the redeem +
// decode primitives back `resolver::JoinTarget::Invite`, and `mint` is `pub`
// for the application layer's `invite` command.
pub mod invite;
pub mod lifecycle;
pub mod logging;
pub mod lookup;
pub mod protocol;
pub mod resolver;
pub mod transport;
pub mod unicast;
pub mod util;
// Graph search + telemetry probes are exercised by unit tests and get wired into
// the send path in later phases; until then they're dead in a non-test build
// only, so the expectation is scoped to that build (a test build uses them
// freely).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "relay graph search / telemetry not yet reachable from production; wired in later phases"
    )
)]
pub mod circuit;

// Re-exported at the crate root so engine code (and the app's re-export) can
// reach the build version stamp as `crate::VERSION`.
pub use util::version::VERSION;

// The `MeshApp` / `MeshDriver` seams are `#[async_trait]`, so any consumer
// implementing them needs the same macro. Re-export it so a downstream crate
// annotates its impls with `#[agent_habilis_gossip::async_trait]` instead of
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
