use crate::SERIAL_GATE;

/// Acquire the [`SERIAL_GATE`]; hold the returned guard for the whole test.
/// Poison-tolerant: a failing (panicking) test must not cascade-poison the
/// gate and spuriously fail the others. For `#[test]` (sync) tests only.
pub fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    SERIAL_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
