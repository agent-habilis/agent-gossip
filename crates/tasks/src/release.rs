use crate::TaskOutcome;
use crate::util::{Invocation, output};

pub(crate) fn run() -> TaskOutcome {
    output::status("Building", "release binary");
    Invocation::cargo(["build", "-p", "agent-gossip", "--release"])
        .quiet()
        .run()?;
    output::status("Built", "target/release/agent-gossip");
    Ok(())
}
