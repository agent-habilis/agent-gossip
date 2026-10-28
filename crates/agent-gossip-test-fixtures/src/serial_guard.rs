use crate::SERIAL_GATE;

/// Acquire the [`SERIAL_GATE`]; hold the returned guard for the whole test.
/// For `#[test]` (sync) tests only.
pub fn serial_guard() -> parking_lot::MutexGuard<'static, ()> {
    SERIAL_GATE.lock()
}
