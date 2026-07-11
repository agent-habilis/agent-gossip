use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    // Two runs at different parallelism (see `ci.rs` for the why): the
    // subprocess reliability suite wants -j2 (daemon oversubscription),
    // the in-process adversarial suite wants -j4 (worker threads keep its
    // mesh responsive). Everything except adversarial at 2, then the
    // adversarial suite alone at 4 (it is `required-features`-gated, so the
    // first run skips it).
    cmd!(sh, "cargo test -p agent-square -- --test-threads=2")
        .quiet()
        .run()?;
    cmd!(
        sh,
        "cargo test -p agent-square --features adversarial --test adversarial -- --test-threads=4"
    )
    .quiet()
    .run()?;

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
