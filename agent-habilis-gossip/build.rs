//! Stamp the build's git identity into compile-time env vars so the binary
//! self-identifies its exact commit (see `src/util/version.rs`):
//! `VERGEN_GIT_SHA` (short hash) and `VERGEN_GIT_DIRTY` ("true"/"false").
//!
//! `idempotent()` keeps a non-git build (released tarball / `cargo install`
//! from crates.io) compiling — the vars are emitted with a placeholder rather
//! than failing the build. vergen also sets the right `rerun-if-changed`
//! (`.git/HEAD` + the active ref) so the stamp never goes stale.
//!
//! This engine crate owns the version stamp because `util::version::VERSION`
//! is an engine module; the app's build script (`../build.rs`) handles the
//! app-only integration-artifact embedding.

use vergen_gitcl::{Emitter, GitclBuilder};

fn main() {
    let Ok(gitcl) = GitclBuilder::default().sha(true).dirty(true).build() else {
        return;
    };
    let _ = Emitter::default()
        .idempotent()
        .add_instructions(&gitcl)
        .and_then(|emitter| emitter.emit());
}
