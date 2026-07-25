//! The single source of truth for the build's version string: the crate
//! version plus the git short hash and dirty flag stamped by `build.rs`
//! (via vergen). Surfaced in `agent-gossip --version`, the `ready` event, and a
//! once-per-daemon "daemon starting" log line (one log file == one process ==
//! one build), so a node self-identifies which commit it is running.

use std::sync::OnceLock;

/// The git stamp alone, e.g. `"(1c362892 dirty:false)"`. Split out so a
/// *consumer* binary can lead with its own crate version — this engine
/// crate's `env!("CARGO_PKG_VERSION")` is the engine's number, and an
/// `agent-gossip --version` that led with it read as a stale release
/// (engine 0.5.0 vs app 0.6.0) during the fossil-stamp incident.
pub const GIT_STAMP: &str = concat!(
    "(",
    env!("VERGEN_GIT_SHA"),
    " dirty:",
    env!("VERGEN_GIT_DIRTY"),
    ")"
);

/// e.g. `"0.2.0 (1c362892 dirty:false)"`. A compile-time `&'static str` (so
/// clap's `#[command(version = …)]` can use it directly). `VERGEN_GIT_*` are
/// always set by `build.rs` (real values from git, or a placeholder for a
/// non-git build), so `env!` never fails here. Re-exported at the crate root
/// (`crate::VERSION`) for the `ready` event and the daemon-start log line.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("VERGEN_GIT_SHA"),
    " dirty:",
    env!("VERGEN_GIT_DIRTY"),
    ")"
);

/// The *consumer* binary's build stamp, registered once at startup. The engine
/// cannot name its consumer's version at compile time — `env!` here yields the
/// engine's own — so the app that links this crate hands its string in and
/// every engine-side surface (the daemon-start log line) reports the number a
/// user would recognize from `--version`.
static BUILD_VERSION: OnceLock<&'static str> = OnceLock::new();

/// Register the consumer's build stamp. Idempotent: the first caller wins and
/// later calls are ignored, so a test that spins up several in-process daemons
/// cannot panic on a re-set.
pub fn set_build_version(version: &'static str) {
    let _ = BUILD_VERSION.set(version);
}

/// The consumer's stamp, or this engine's own [`VERSION`] when nothing
/// registered one. The fallback keeps a direct engine embedder (e.g.
/// `examples/mesh-pipe`) truthful rather than blank.
#[must_use]
pub fn build_version() -> &'static str {
    BUILD_VERSION.get().copied().unwrap_or(VERSION)
}

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_carries_crate_version_and_git_stamp() {
        assert!(
            VERSION.starts_with(env!("CARGO_PKG_VERSION")),
            "version must lead with the crate version: {VERSION}"
        );
        assert!(
            VERSION.contains("dirty:"),
            "version must carry the git dirty flag: {VERSION}"
        );
    }
}
