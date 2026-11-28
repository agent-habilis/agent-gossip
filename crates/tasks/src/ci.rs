use crate::TaskOutcome;
use crate::util::{Invocation, output};

pub(crate) fn run() -> TaskOutcome {
    output::status("Checking", "formatting");
    crate::fmt::check()?;

    output::status("Running", "clippy");
    crate::lint::run()?;

    // The `bench` feature builds the bench shim and the benches, which no other
    // step compiles.
    output::status("Checking", "benches");
    Invocation::cargo([
        "check",
        "-p",
        "agent-gossip",
        "--features",
        "bench",
        "--benches",
    ])
    .quiet()
    .run()?;

    // `-D warnings` turns the `[workspace.lints.rustdoc]` levels and every other rustdoc warning
    // into an error. `--document-private-items` so a broken link in the doc of a private item
    // fails too: most items of this workspace are private, and rustdoc skips them otherwise.
    // The two features bring in the `harness` shims, whose docs rustdoc skips without them.
    output::status("Running", "rustdoc");
    Invocation::cargo([
        "doc",
        "--workspace",
        "--no-deps",
        "--document-private-items",
        "--features",
        "agent-gossip/bench,agent-gossip/adversarial",
    ])
    .env("RUSTDOCFLAGS", "-D warnings")
    .quiet()
    .run()?;

    // One implementation, not a second copy: `test` owns the profile choice, the
    // per-leg thread counts, and the concurrent split. Forking those here is
    // exactly how the two drifted before.
    //
    // The reliability invariants (ungraceful SIGKILL death, sleep/wake heal
    // recovery, state backfill) run every time — they live in
    // `tests/gossip_network.rs` with shortened eviction timers.
    crate::test::run()
}
