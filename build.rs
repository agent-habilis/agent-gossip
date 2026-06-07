//! Stamp the build's git identity into compile-time env vars so the binary
//! self-identifies its exact commit (see `src/util/version.rs`):
//! `VERGEN_GIT_SHA` (short hash) and `VERGEN_GIT_DIRTY` ("true"/"false").
//!
//! `idempotent()` keeps a non-git build (released tarball / `cargo install`
//! from crates.io) compiling — the vars are emitted with a placeholder rather
//! than failing the build. vergen also sets the right `rerun-if-changed`
//! (`.git/HEAD` + the active ref) so the stamp never goes stale.
//!
//! It also **stages a filtered copy of `pi-extension/`** into `OUT_DIR` so
//! `src/cli/setup.rs` can `include_dir!` the TS source *without* the 200 MB+
//! local `node_modules/` (gitignored, present after `bun install`) being baked
//! into the binary, and emits an **embed fingerprint** so editing any embedded
//! artifact forces a rebuild (`include_dir!` is otherwise untracked on stable).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use vergen_gitcl::{Emitter, GitclBuilder};

/// Names never staged/embedded — shared verbatim with `src/cli/setup.rs`'s
/// write-out filter via one `include!`d fragment, so staging and write-out can
/// never disagree (the binary never carries a file it would discard on install).
const SKIP: &[&str] = include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/cli/embed_skip.rs"
));

/// The repo dirs `src/cli/setup.rs` embeds, relative to the manifest.
const EMBED_DIRS: &[&str] = &["claude-code-plugin", "pi-extension", "skills/swarm"];

fn main() {
    // Embed staging + fingerprint run first so they execute even on the
    // non-git early-return path below — `setup.rs`'s `env!` needs the var.
    stage_pi_extension();
    emit_embed_fingerprint();

    let Ok(gitcl) = GitclBuilder::default().sha(true).dirty(true).build() else {
        return;
    };
    let _ = Emitter::default()
        .idempotent()
        .add_instructions(&gitcl)
        .and_then(|emitter| emitter.emit());
}

/// Copy `pi-extension/` → `$OUT_DIR/pi-extension/`, skipping [`SKIP`], so
/// `node_modules` is never embedded regardless of local `bun install` state.
fn stage_pi_extension() {
    let out = std::env::var_os("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out).join("pi-extension");
    // Fresh copy each run so a deleted source file doesn't linger in the stage.
    let _ = std::fs::remove_dir_all(&dest);
    copy_filtered(Path::new("pi-extension"), &dest);
}

fn copy_filtered(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).expect("create staged dir");
    let entries = std::fs::read_dir(src).expect("read pi-extension");
    for entry in entries.flatten() {
        let name = entry.file_name();
        if SKIP.iter().any(|skip| name == **skip) {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        if from.is_dir() {
            copy_filtered(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy staged file");
        }
    }
}

/// Hash the (skip-filtered) contents of every embedded dir and publish it as
/// `AHS_EMBED_FINGERPRINT`. `setup.rs` reads it via `env!`, so a changed
/// fingerprint recompiles that module and re-expands the embeds; the
/// `rerun-if-changed` lines make this script recompute when a source changes.
fn emit_embed_fingerprint() {
    let mut hasher = DefaultHasher::new();
    for dir in EMBED_DIRS {
        println!("cargo:rerun-if-changed={dir}");
        hash_dir(Path::new(dir), &mut hasher);
    }
    println!(
        "cargo:rustc-env=AHS_EMBED_FINGERPRINT={:016x}",
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
