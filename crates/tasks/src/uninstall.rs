use crate::TaskOutcome;
use crate::util::{Invocation, output};

pub(crate) fn run() -> TaskOutcome {
    output::status("Uninstalling", "agent-gossip");
    Invocation::cargo(["uninstall", "agent-gossip"])
        .quiet()
        .run()?;
    Ok(())
}
