use super::{Invocation, ensure_installed, output};

/// Prune `target/` artifacts not touched in the last week. Old build
/// generations from past sessions pile up (feature-flag/profile permutations
/// and `[patch]` source swaps have pushed this tree to tens of GB), while the
/// build that just ran is touched *now* and is always kept. Best-effort:
/// installs `cargo-sweep` on demand and never aborts the calling task.
pub(crate) fn sweep_stale_artifacts() {
    ensure_installed("cargo-sweep", &["sweep", "--version"]);
    output::status("Pruning", "build artifacts older than 7 days");
    let _ = Invocation::cargo(["sweep", "--time", "7"]).quiet().run();
}
