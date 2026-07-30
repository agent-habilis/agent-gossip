use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    // `--profile ci` (release-derived, LTO off) rather than dev, for two reasons.
    //
    // Speed: debug-built curve/AEAD/hash math is ~10x slower and every test here
    // is handshake-bound, so the profile — not parallelism — is what dominates
    // wall time.
    //
    // Fidelity: it makes a local run match GitHub CI, which already uses this
    // profile. `profile.ci` has `debug_assertions` off, which flips
    // `logging::log_filter`'s base level from `info` to `error`; that divergence
    // once let a log-asserting test pass locally while failing 100% in CI.
    //
    // The trade: the one `debug_assert!` (in `lookup`) no longer fires here.
    output::status("Building", "test binaries (ci profile)");
    // `cargo build` as well as `cargo test --no-run`, under the same profile:
    // `cargo test` builds test targets plus the bins an integration test execs,
    // but it does **not** emit a `cdylib`. `agent-habilis-mesh-ffi` is
    // `["cdylib", "rlib"]` and its C suite links that cdylib out of
    // `target/<profile>/`, so without this its tests fail on a missing artifact.
    cmd!(sh, "cargo build --workspace --profile ci")
        .quiet()
        .run()?;

    // Two runs at different parallelism: the suites want opposite thread counts.
    // The subprocess reliability tests (`gossip_network.rs`: SIGSTOP storms,
    // beacon migration, anti-entropy, flap storms) oversubscribe the host at -j4
    // and starve each other's heal-cadence-gated recovery, so they want -j2. The
    // in-process adversarial suite is the reverse — it needs the worker threads
    // higher parallelism provides to keep its mesh responsive, and flakes at -j2.
    // So: everything except adversarial at 2, then adversarial alone at 4 (it is
    // `required-features`-gated, so the first run skips it).
    //
    // These run **sequentially, and must stay that way.** Splitting them across
    // concurrent processes was tried and reverted: it is not merely -j4 by another
    // name, it is worse — `gossip_network` alongside the other 30 binaries (many
    // of which spawn their own daemons) starved
    // `test_flap_storm_all_rosters_recover` into a real failure. GitHub CI shards
    // these binaries only because each shard gets its own *machine*; locally every
    // leg shares one host. Nor can `gossip_network` be split within itself:
    // `serial_guard()` is a process-local mutex, so two processes of the same
    // binary would each take their own gate and not serialize at all.
    //
    // `--workspace`, not `-p agent-gossip`: the engine crate holds the wire
    // version, every crypto byte-domain and `runtime_base()`, and its snapshots
    // pin the frame format. Scoped to the app, none of that was ever tested and a
    // stale engine snapshot stayed green. `--no-fail-fast` because cargo otherwise
    // stops at the first failing binary and leaves every later one unreported,
    // which reads as "only one thing broke".
    cmd!(
        sh,
        "cargo test --workspace --profile ci --no-fail-fast -- --test-threads=2"
    )
    .quiet()
    .run()?;
    cmd!(
        sh,
        "cargo test -p agent-gossip --features adversarial --test adversarial --profile ci --no-fail-fast -- --test-threads=4"
    )
    .quiet()
    .run()?;

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
