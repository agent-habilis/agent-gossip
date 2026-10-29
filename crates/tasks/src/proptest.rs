use crate::TaskOutcome;
use crate::util::{Invocation, output};

pub(crate) fn run() -> TaskOutcome {
    output::status("Running", "property-based tests (prop_)");
    Invocation::cargo(["test", "-p", "agent-gossip", "prop_"])
        .quiet()
        .run()?;
    Ok(())
}
