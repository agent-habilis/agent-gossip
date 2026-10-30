use std::process::Child;
use std::time::Instant;

use crate::{MSG_TIMEOUT, POLL};

/// Wait for a bell to ring within [`MSG_TIMEOUT`] and return its stdout.
///
/// # Panics
///
/// Panics if the bell does not exit before `MSG_TIMEOUT`, or if its output is unreadable.
pub fn wait_bell_output(bell: &mut Child) -> String {
    use std::io::Read as _;

    let deadline = Instant::now() + MSG_TIMEOUT;
    while bell
        .try_wait()
        .expect("polling a child that we own cannot fail unless the OS lost the process")
        .is_none()
    {
        assert!(Instant::now() < deadline, "bell never rang");
        std::thread::sleep(POLL);
    }
    let mut body = String::new();
    bell.stdout
        .take()
        .expect("bell stdout is piped")
        .read_to_string(&mut body)
        .expect("the bell has exited, so its piped output is complete and valid UTF-8");
    body
}
