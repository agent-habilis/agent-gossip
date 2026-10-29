use crate::TaskOutcome;
use crate::util::Invocation;

pub(crate) fn run() -> TaskOutcome {
    Invocation::cargo(["fmt", "--all"]).quiet().run()?;
    Ok(())
}

pub(crate) fn check() -> TaskOutcome {
    Invocation::cargo(["fmt", "--all", "--check"])
        .quiet()
        .run()?;
    Ok(())
}
