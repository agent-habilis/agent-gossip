use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo fmt --all").quiet().run()?;
    Ok(())
}

pub(crate) fn check(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo fmt --all --check").quiet().run()?;
    Ok(())
}
