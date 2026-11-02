#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsHandle;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};

#[cfg(windows)]
use super::keep_from_child::keep_from_child;

/// The detached daemon's copy of one of the launcher's output streams. A pipe or
/// a socket is how a caller captures output (libuv, and so Node, uses a
/// socketpair), and the caller reads it to EOF: a daemon that held the write
/// end would block the caller for its whole life and fill the pipe with message
/// bodies. A file, `/dev/null` or a tty is inherited, so the skill's `.stderr`
/// file still gets startup errors.
#[cfg(unix)]
pub(super) fn detached_stdio(stream: &impl AsFd) -> std::process::Stdio {
    use std::os::unix::fs::FileTypeExt as _;
    let owned = stream.as_fd().try_clone_to_owned();
    let metadata = owned.and_then(|fd| std::fs::File::from(fd).metadata());
    let captured: bool = metadata.map_or(true, |meta| {
        meta.file_type().is_fifo() || meta.file_type().is_socket()
    });
    if captured {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::inherit()
    }
}

/// The detached daemon's copy of one of the launcher's output streams, on
/// Windows. Only a file on disk is inherited, so the skill's `.stderr` file
/// still gets startup errors. A pipe is how a caller captures output, as on
/// Unix. A console handle means nothing to a process with no console. Each
/// of them, and a handle whose type cannot be read, gives null, and the
/// handle is kept from the daemon (`keep_from_child`).
#[cfg(windows)]
pub(super) fn detached_stdio(stream: &impl AsHandle) -> std::process::Stdio {
    use std::os::windows::io::AsRawHandle as _;

    let handle = stream.as_handle();
    #[expect(
        unsafe_code,
        reason = "GetFileType has no safe std wrapper, and std::fs::File::metadata does not tell a pipe from a console"
    )]
    // SAFETY: `GetFileType` only reads the type of a handle that `handle`
    // borrows, so the handle stays open for the call.
    let kind = unsafe { GetFileType(handle.as_raw_handle()) };
    if kind == FILE_TYPE_DISK {
        std::process::Stdio::inherit()
    } else {
        keep_from_child(handle);
        std::process::Stdio::null()
    }
}
