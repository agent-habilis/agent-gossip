use std::path::{Path, PathBuf};

/// Workspace root: the grandparent of this crate's `crates/tasks/` manifest dir.
/// Falls back to CWD if the env var is somehow missing.
pub(crate) fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/tasks/, whose grandparent is the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            Path::to_path_buf,
        )
}
