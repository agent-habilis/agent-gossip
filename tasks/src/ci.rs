use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Checking", "formatting");
    cmd!(sh, "cargo fmt --check").quiet().run()?;

    output::status("Running", "clippy");
    // `--features adversarial` so the adversarial suite + shim are linted
    // too (they are `required-features`-gated, else clippy would skip them).
    cmd!(
        sh,
        "cargo clippy --all-targets --features adversarial -- -D warnings"
    )
    .quiet()
    .run()?;

    output::status("Running", "tests");
    // Two runs at different parallelism: the suites want opposite thread
    // counts. The subprocess reliability tests (`gossip_network.rs`:
    // SIGSTOP storms, beacon migration, anti-entropy) oversubscribe the
    // host at -j4 and starve each other's heal-cadence-gated recovery
    // (they inject a short `--heal-interval-secs`, but the recovery
    // windows still contend), so they want -j2. The in-process
    // adversarial suite is the reverse — it needs the worker threads that
    // higher parallelism provides to keep its mesh responsive, and flakes
    // at -j2 — so it wants -j4. Daemons mesh fine either way (verified
    // out-of-band); this is purely test-host scheduling. So: everything
    // except adversarial at 2, then the adversarial suite alone at 4.
    // (GitHub CI runs only the first — it does not enable the
    // `adversarial` feature.)
    cmd!(sh, "cargo test -- --test-threads=2").quiet().run()?;
    cmd!(
        sh,
        "cargo test --features adversarial --test adversarial -- --test-threads=4"
    )
    .quiet()
    .run()?;

    // The reliability invariants (ungraceful SIGKILL death, sleep/wake
    // heal recovery, anti-entropy backfill) run here every time — they
    // live in `tests/gossip_network.rs` with shortened eviction timers.

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
