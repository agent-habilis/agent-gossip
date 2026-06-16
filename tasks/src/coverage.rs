use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{ensure_installed, output};

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    ensure_installed(sh, "cargo-llvm-cov", &["llvm-cov", "--version"]);
    output::status("Running", "tests with coverage");
    // `--test-threads=2` bounds concurrent daemons the same as
    // `test`/`ci` — `cargo llvm-cov` wraps `cargo test`, so the
    // integration suites would otherwise over-subscribe and flake.
    cmd!(sh, "cargo llvm-cov --no-report -- --test-threads=2")
        .quiet()
        .run()?;
    output::status("Coverage", "summary");
    cmd!(sh, "cargo llvm-cov report").quiet().run()?;
    Ok(())
}
