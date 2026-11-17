use crate::TaskOutcome;
use crate::util::{Invocation, output};

pub(crate) fn run() -> TaskOutcome {
    output::status("Checking", "formatting");
    crate::fmt::check()?;

    // No layering gate any more: it checked that the engine crate named neither
    // A2A nor the app, and the engine now lives in its own repo (`habilis-network`), so
    // the boundary is enforced by the repo edge rather than by a grep.

    // The `style` job of ci.yml runs these checkers, and no lint covers their rules, so the gate
    // runs them too: a violation fails here and not first on the pull request. A checker runs
    // before its own tests because it is the fast one.
    for checker in [
        "style-check",
        "script-language",
        "macro-rules",
        "pub-trait",
        "module-layout",
        "naming",
    ] {
        output::status("Checking", checker);
        Invocation::new("bun")
            .args([format!("scripts/{checker}/{checker}.ts")])
            .quiet()
            .run()?;
        Invocation::new("bun")
            .args([
                "test".to_owned(),
                format!("scripts/{checker}/{checker}.test.ts"),
            ])
            .quiet()
            .run()?;
    }
    // The last step of the `style` job has no checker: its test runs rustdoc and clippy on a scratch
    // crate.
    output::status("Checking", "rustdoc-lints");
    Invocation::new("bun")
        .args(["test", "scripts/rustdoc-lints/rustdoc-lints.test.ts"])
        .quiet()
        .run()?;

    output::status("Running", "clippy");
    // `--features adversarial` so the adversarial suite + shim are linted
    // too (they are `required-features`-gated, else clippy would skip them).
    // See `lint.rs` for why the feature is qualified and why this is
    // `--workspace`.
    Invocation::cargo([
        "clippy",
        "--workspace",
        "--all-targets",
        "--features",
        "agent-gossip/adversarial",
        "--",
        "-D",
        "warnings",
    ])
    .quiet()
    .run()?;

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
