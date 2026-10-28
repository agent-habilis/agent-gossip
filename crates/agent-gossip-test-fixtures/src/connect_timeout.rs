use std::time::Duration;

/// How long a subprocess node may take to bind its IPC socket before a test gives up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);
