use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Running property-based tests (prop_ prefix)...");
    cmd!(sh, "cargo test prop_").quiet().run()?;
    Ok(())
}
