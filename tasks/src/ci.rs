use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Checking formatting...");
    cmd!(sh, "cargo fmt --check").quiet().run()?;

    eprintln!("=> Running clippy...");
    cmd!(sh, "cargo clippy --all-targets -- -D warnings")
        .quiet()
        .run()?;

    eprintln!("=> Running tests...");
    cmd!(sh, "cargo test -- --test-threads=4").quiet().run()?;

    // The reliability invariants (ungraceful SIGKILL death, sleep/wake
    // heal recovery, anti-entropy backfill) run here every time — they
    // live in `tests/gossip_network.rs` with shortened eviction timers.

    crate::pi::typecheck(sh)?;
    crate::pi::lint(sh)
}
