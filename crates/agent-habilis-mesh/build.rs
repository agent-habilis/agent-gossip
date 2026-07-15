//! Stamp the build's git identity into compile-time env vars so the binary
//! self-identifies its exact commit (see `src/util/version.rs`):
//! `VERGEN_GIT_SHA` (short hash) and `VERGEN_GIT_DIRTY` ("true"/"false").
//!
//! Deliberately **not** `Emitter::idempotent()`: vergen emits the
//! `rerun-if-changed` directives for `.git/HEAD` + the active ref only when
//! idempotent is off (vergen-gitcl `inner_add_git_map_entries`). With the flag
//! on, nothing invalidates the cached build-script output, and any reused
//! target dir — `cargo install --path` reuses the workspace `target/release` —
//! serves a fossilized stamp forever: installs kept reporting `765793f` for
//! days of newer commits. A non-git build (released tarball) still compiles
//! without the flag: the emitter's default error path (`fail_on_error` off)
//! emits placeholder values for the requested keys instead of failing.

use vergen_gitcl::{Emitter, GitclBuilder};

fn main() {
    let Ok(gitcl) = GitclBuilder::default().sha(true).dirty(true).build() else {
        return;
    };
    let _ = Emitter::default()
        .add_instructions(&gitcl)
        .and_then(|emitter| emitter.emit());
}
