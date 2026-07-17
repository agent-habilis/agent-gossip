// Names never embedded or written by the mesh-integration installer — build
// cruft and pi's local deps. Shared via `include!` by **both** `build.rs`
// (staging + fingerprint) and `src/cli/setup.rs` (write-out) so the two can
// never drift. This is an `include!` fragment, not a module: it expands to a
// `&[&str]` slice expression, so don't `mod` it.
&[
    "node_modules",
    ".git",
    ".DS_Store",
    "bun.lock",
    ".claude",
    "biome.json",
    "AGENTS.md",
    "README.md",
]
