use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Checking", "formatting");
    cmd!(sh, "cargo fmt --check").quiet().run()?;

    output::status("Running", "clippy");
    // `--features testkit` so the adversarial suite + testkit shim are linted
    // too (they are `required-features`-gated, else clippy would skip them).
    cmd!(
        sh,
        "cargo clippy --all-targets --features testkit -- -D warnings"
    )
    .quiet()
    .run()?;

    output::status("Running", "tests");
    cmd!(sh, "cargo test --features testkit -- --test-threads=4")
        .quiet()
        .run()?;

    // The reliability invariants (ungraceful SIGKILL death, sleep/wake
    // heal recovery, anti-entropy backfill) run here every time — they
    // live in `tests/gossip_network.rs` with shortened eviction timers.

    crate::pi::typecheck(sh)?;
    crate::pi::lint(sh)?;

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
