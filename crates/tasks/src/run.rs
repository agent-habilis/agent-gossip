use crate::TaskOutcome;
use crate::util::Invocation;

pub(crate) fn run(args: &[String]) -> TaskOutcome {
    Invocation::cargo(["run", "-p", "agent-gossip", "--"])
        .args(args)
        .run()?;
    Ok(())
}
