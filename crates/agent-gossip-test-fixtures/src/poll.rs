use std::time::Duration;

/// Sleep between probes in the adaptive wait loops.
pub const POLL: Duration = Duration::from_millis(250);
