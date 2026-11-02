use std::time::{Duration, Instant};

use crate::POLL;

/// Wait until `count_fn` returns >= `target` or `timeout` elapses.
pub fn wait_until(count_fn: impl Fn() -> usize, target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let current = count_fn();
        if current >= target || Instant::now() > deadline {
            return current;
        }
        std::thread::sleep(POLL);
    }
}
