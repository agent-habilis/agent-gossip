use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use crate::{CONNECT_TIMEOUT, POLL};

/// Poll the session state file until it reports `ready` with a bound A2A
/// endpoint; returns `(port, token)`. The daemon writes the two keys in one step.
///
/// # Panics
///
/// Panics if the state file reports no bound endpoint before `CONNECT_TIMEOUT`.
#[must_use]
pub fn wait_a2a(state_file: &Path) -> (u16, String) {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(raw) = std::fs::read_to_string(state_file)
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw)
            && parsed["ready"] == true
            && let (Some(port), Some(token)) =
                (parsed["a2a_port"].as_u64(), parsed["a2a_token"].as_str())
        {
            return (
                u16::try_from(port).expect("port fits u16"),
                token.to_string(),
            );
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "daemon never reported a bound a2a endpoint in {}",
        state_file.display()
    );
}
