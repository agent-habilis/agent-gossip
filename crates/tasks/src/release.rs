use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Building", "release binary");
    cmd!(sh, "cargo build -p agent-gossip --release")
        .quiet()
        .run()?;
    output::status("Built", "target/release/agent-gossip");
    Ok(())
}
