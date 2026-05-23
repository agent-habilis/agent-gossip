use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo test -- --test-threads=4").quiet().run()?;
    Ok(())
}
