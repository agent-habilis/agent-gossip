use std::path::{Path, PathBuf};

use xshell::{Shell, cmd};

#[expect(
    dead_code,
    reason = "shared cargo-style helpers included from src/cli/output.rs; the task runner uses a subset"
)]
pub(crate) mod output;

/// Workspace root: the parent of this crate's `tasks/` manifest dir.
/// Falls back to CWD if the env var is somehow missing.
pub(crate) fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is tasks/, whose parent is the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            Path::to_path_buf,
        )
}

/// Install `krate` via `cargo install --locked` if the probe command
/// (`check`) fails. Best-effort: a probe or install hiccup must not
/// abort the calling task — its own command surfaces a clear error if
/// the tool is genuinely missing.
pub(crate) fn ensure_installed(sh: &Shell, krate: &str, check: &[&str]) {
    let ok = cmd!(sh, "cargo {check...}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();

    if !ok {
        output::status("Installing", krate);
        let _ = cmd!(sh, "cargo install --locked {krate}").quiet().run();
    }
}

/// Prune `target/` artifacts not touched in the last week. Old build
/// generations from past sessions pile up (feature-flag/profile permutations
/// and `[patch]` source swaps have pushed this tree to tens of GB), while the
/// build that just ran is touched *now* and is always kept. Best-effort:
/// installs `cargo-sweep` on demand and never aborts the calling task.
pub(crate) fn sweep_stale_artifacts(sh: &Shell) {
    ensure_installed(sh, "cargo-sweep", &["sweep", "--version"]);
    output::status("Pruning", "build artifacts older than 7 days");
    let _ = cmd!(sh, "cargo sweep --time 7").quiet().run();
}
