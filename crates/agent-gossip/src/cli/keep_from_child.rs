use std::os::windows::io::BorrowedHandle;

use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

/// Clear the inherit flag of a handle of the launcher. std starts a child with
/// `bInheritHandles`, so an inheritable handle of the launcher reaches the
/// daemon even when the daemon gets null in its place, and a pipe of the caller
/// would then never reach its end. A null or invalid handle makes the call
/// fail, and the failure is ignored: there is then nothing to inherit.
pub(super) fn keep_from_child(handle: BorrowedHandle<'_>) {
    use std::os::windows::io::AsRawHandle as _;

    #[expect(
        unsafe_code,
        reason = "std has no safe way to stop a handle from being inherited"
    )]
    // SAFETY: `SetHandleInformation` only changes the inherit flag of a handle
    // that `handle` borrows, so the handle stays open for the call.
    unsafe {
        SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, 0);
    }
}
