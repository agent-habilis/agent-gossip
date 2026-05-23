use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo fmt").quiet().run()?;
    Ok(())
}
