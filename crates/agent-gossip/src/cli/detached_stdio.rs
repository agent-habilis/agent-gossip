use std::os::fd::AsFd;
/// The detached daemon's copy of one of the launcher's output streams. A pipe or
/// a socket is how a caller captures output (libuv, and so Node, uses a
/// socketpair), and the caller reads it to EOF: a daemon that held the write
/// end would block the caller for its whole life and fill the pipe with message
/// bodies. A file, `/dev/null` or a tty is inherited, so the skill's `.stderr`
/// file still gets startup errors.
pub(super) fn detached_stdio(stream: &impl AsFd) -> std::process::Stdio {
    use std::os::unix::fs::FileTypeExt as _;
    let captured: bool = stream
        .as_fd()
        .try_clone_to_owned()
        .and_then(|fd| std::fs::File::from(fd).metadata())
        .map_or(true, |meta| {
            meta.file_type().is_fifo() || meta.file_type().is_socket()
        });
    if captured {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::inherit()
    }
}
