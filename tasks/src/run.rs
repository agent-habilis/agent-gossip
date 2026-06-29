use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell, args: &[String]) -> TaskOutcome {
    cmd!(sh, "cargo run -- {args...}").run()?;
    Ok(())
}
