use crate::{MSG_TIMEOUT, wait_until};

/// [`wait_until`] with the standard message-delivery timeout.
pub fn wait_total(total_fn: impl Fn() -> usize, target: usize) -> usize {
    wait_until(total_fn, target, MSG_TIMEOUT)
}
