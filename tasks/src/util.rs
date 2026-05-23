use xshell::{Shell, cmd};

/// Workspace root: the parent of this crate's `tasks/` manifest dir.
/// Falls back to CWD if the env var is somehow missing.
pub(crate) fn repo_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is tasks/, whose parent is the workspace root.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            std::path::Path::to_path_buf,
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
        eprintln!("=> Installing {krate}...");
        let _ = cmd!(sh, "cargo install --locked {krate}").quiet().run();
    }
}
