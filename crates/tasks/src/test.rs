use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    // Two runs at different parallelism (see `ci.rs` for the why): the
    // subprocess reliability suite wants -j2 (daemon oversubscription),
    // the in-process adversarial suite wants -j4 (worker threads keep its
    // mesh responsive). Everything except adversarial at 2, then the
    // adversarial suite alone at 4 (it is `required-features`-gated, so the
    // first run skips it).
    //
    // `--workspace`, not `-p agent-gossip`: the engine crate holds the wire
    // version, every crypto byte-domain and `runtime_base()`, and its snapshots
    // pin the frame format. Scoped to the app, none of that was ever tested and
    // a stale engine snapshot stayed green. `--no-fail-fast` because cargo
    // otherwise stops at the first failing binary and leaves every later one
    // unreported, which reads as "only one thing broke".
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

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
