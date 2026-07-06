//! The single source of truth for the build's version string: the crate
//! version plus the git short hash and dirty flag stamped by `build.rs`
//! (via vergen). Surfaced in `agent-mesh --version`, the `ready` event, and a
//! once-per-daemon "daemon starting" log line (one log file == one process ==
//! one build), so a node self-identifies which commit it is running.

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
