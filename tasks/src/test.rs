use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    // `testkit` enables the adversarial suite (`tests/adversarial.rs`), which
    // is `required-features`-gated so a bare `cargo test` skips it.
    cmd!(sh, "cargo test --features testkit -- --test-threads=4")
        .quiet()
        .run()?;

    crate::util::sweep_stale_artifacts(sh);
    Ok(())
}
