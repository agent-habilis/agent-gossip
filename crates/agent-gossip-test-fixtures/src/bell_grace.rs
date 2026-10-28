use std::time::Duration;

/// How long to let a bell run before concluding it did not fire — comfortably
/// over the CLI's `POLL_LONG_MIN_CYCLE_MS` re-issue cycle, so a bell that fired
/// on the very first round has exited by the time we look.
pub const BELL_GRACE: Duration = Duration::from_secs(3);
