use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Checking", "formatting");
    cmd!(sh, "cargo fmt --all --check").quiet().run()?;

    // Cheap and first: a layering leak is a design regression, and finding it
    // before the multi-minute clippy+test legs keeps the feedback fast.
    crate::layering::run()?;

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

    // One implementation, not a second copy: `test` owns the profile choice, the
    // per-leg thread counts, and the concurrent split. Forking those here is
    // exactly how the two drifted before.
    //
    // The reliability invariants (ungraceful SIGKILL death, sleep/wake heal
    // recovery, anti-entropy backfill) run every time — they live in
    // `tests/gossip_network.rs` with shortened eviction timers.
    crate::test::run(sh)
}
