use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Running", "property-based tests (prop_)");
    cmd!(sh, "cargo test -p agent-gossip prop_").quiet().run()?;
    Ok(())
}
