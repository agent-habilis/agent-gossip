use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Uninstalling", "agent-gossip");
    cmd!(sh, "cargo uninstall agent-gossip").quiet().run()?;
    Ok(())
}
