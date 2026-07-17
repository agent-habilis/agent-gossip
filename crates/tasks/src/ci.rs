use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Checking", "formatting");
    cmd!(sh, "cargo fmt --all --check").quiet().run()?;

    output::status("Running", "clippy");
    // `--features adversarial` so the adversarial suite + shim are linted
    // too (they are `required-features`-gated, else clippy would skip them).
    // See `lint.rs` for why the feature is qualified and why this is
    // `--workspace`.
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features agent-gossip/adversarial -- -D warnings"
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
    //
    // `--workspace` covers `agent-habilis-mesh`, which owns the wire version,
    // the crypto byte-domains and `runtime_base()` — the highest-risk code in
    // the tree, and untested here until it was scoped in. The engine is
    // in-process and finishes in seconds, so -j2 costs it nothing.
    // `--no-fail-fast` so one red binary does not hide every binary after it.
    cmd!(
        sh,
        "cargo test --workspace --no-fail-fast -- --test-threads=2"
    )
    .quiet()
    .run()?;
    cmd!(
        sh,
        "cargo test -p agent-gossip --features adversarial --test adversarial --no-fail-fast -- --test-threads=4"
    )
    .quiet()
    .run()?;

    // The reliability invariants (ungraceful SIGKILL death, sleep/wake
    // heal recovery, anti-entropy backfill) run here every time — they
    // live in `tests/gossip_network.rs` with shortened eviction timers.

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
