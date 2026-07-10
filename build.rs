//! Stage the integration artifacts the `agent-square setup` installer embeds.
//!
//! The installer embeds the portable `skills/` tree and emits an embed
//! fingerprint so editing any embedded artifact forces a rebuild (`include_dir!`
//! is otherwise untracked on stable).
//!
//! The git version stamp (`VERGEN_GIT_*`, feeding `util::version::VERSION`)
//! lives in the engine crate's build script (`agent-habilis-mesh/build.rs`),
//! since `util::version` is an engine module.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Names never staged/embedded — shared verbatim with `src/cli/setup.rs`'s
/// write-out filter via one `include!`d fragment, so staging and write-out can
/// never disagree (the binary never carries a file it would discard on install).
const SKIP: &[&str] = include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/cli/embed_skip.rs"
));

/// The repo dirs `src/cli/setup.rs` embeds, relative to the manifest.
const EMBED_DIRS: &[&str] = &["skills"];

fn main() {
    emit_embed_fingerprint();
}

/// Hash the (skip-filtered) contents of every embedded dir and publish it as
/// `AGENT_SQUARE_EMBED_FINGERPRINT`. `setup.rs` reads it via `env!`, so a changed
/// fingerprint recompiles that module and re-expands the embeds; the
/// `rerun-if-changed` lines make this script recompute when a source changes.
fn emit_embed_fingerprint() {
    let mut hasher = DefaultHasher::new();
    for dir in EMBED_DIRS {
        println!("cargo:rerun-if-changed={dir}");
        hash_dir(Path::new(dir), &mut hasher);
    }
    println!(
        "cargo:rustc-env=AGENT_SQUARE_EMBED_FINGERPRINT={:016x}",
        hasher.finish()
    );
}

/// Feed `dir`'s skip-filtered tree (paths + file bytes) into `hasher`, in a
/// deterministic order so the fingerprint is stable across builds.
fn hash_dir(dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if SKIP.iter().any(|skip| entry.file_name() == **skip) {
            continue;
        }
        let path = entry.path();
        path.to_string_lossy().hash(hasher);
        if path.is_dir() {
            hash_dir(&path, hasher);
        } else if let Ok(bytes) = std::fs::read(&path) {
            bytes.hash(hasher);
        }
    }
}
