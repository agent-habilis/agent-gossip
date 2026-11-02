use crate::TaskOutcome;
use crate::util::{Invocation, ensure_installed, output};

pub(crate) fn run() -> TaskOutcome {
    ensure_installed("cargo-llvm-cov", &["llvm-cov", "--version"]);
    output::status("Running", "tests with coverage");
    // `--test-threads=2` bounds concurrent daemons the same as
    // `test`/`ci` — `cargo llvm-cov` wraps `cargo test`, so the
    // integration suites would otherwise over-subscribe and flake.
    Invocation::cargo([
        "llvm-cov",
        "-p",
        "agent-gossip",
        "--no-report",
        "--",
        "--test-threads=2",
    ])
    .quiet()
    .run()?;
    output::status("Coverage", "summary");
    Invocation::cargo(["llvm-cov", "report"]).quiet().run()?;
    Ok(())
}
