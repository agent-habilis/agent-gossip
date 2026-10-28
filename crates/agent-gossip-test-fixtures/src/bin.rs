use std::path::PathBuf;

/// The freshly built `agent-gossip` binary under test (never a stale release
/// build, whose output formats may differ).
///
/// Resolved from the *running* test executable rather than
/// `env!("CARGO_BIN_EXE_agent-gossip")`: cargo defines that variable only while
/// compiling the owning package's integration tests and benches, never for a
/// library — not even a path dev-dependency of that same package — so the
/// compile-time form cannot live in this crate. libtest puts the test binary at
/// `<target>/<profile>/deps/<name>-<hash>`; the app sits one level up.
pub fn bin() -> PathBuf {
    let mut dir = std::env::current_exe().expect("path of the running test executable");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join("agent-gossip")
}
