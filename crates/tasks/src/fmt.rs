use xshell::{Shell, cmd};

use crate::TaskOutcome;

// `rustfmt.toml` sets `group_imports`, which only nightly rustfmt honours.
// Pinned by date because unstable rustfmt options change between nightlies,
// and `.github/workflows/ci.yml` names the same toolchain for its fmt job.
const NIGHTLY: &str = "+nightly-2026-06-05";

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo {NIGHTLY} fmt --all").quiet().run()?;
    Ok(())
}

pub(crate) fn check(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo {NIGHTLY} fmt --all --check")
        .quiet()
        .run()?;
    Ok(())
}
