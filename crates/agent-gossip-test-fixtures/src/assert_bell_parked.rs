use std::process::Child;

/// Fail with `why` when the bell has already exited, that is, when it fired.
///
/// # Panics
///
/// Panics if the bell already exited.
pub fn assert_bell_parked(bell: &mut Child, why: &str) {
    assert!(
        bell.try_wait()
            .expect("polling a child that we own cannot fail unless the OS lost the process")
            .is_none(),
        "{why}"
    );
}
