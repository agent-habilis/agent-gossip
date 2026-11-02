use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::Result;
use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};

use super::args::SharedServerOpts;
use super::keep_from_child::keep_from_child;

/// With `--stderr-file` on Windows, make the session file the stderr of this
/// process before anything else runs, as the `2>` of the shell does on Unix:
/// an error found while the mesh and the password are resolved lands there,
/// and an old file is never shown as the reason. A detached daemon inherits
/// it from its launcher, which leaves the flag out of the daemon's arguments.
pub(super) fn stderr_to_session_file(shared: &SharedServerOpts) -> Result<()> {
    match shared.stderr_file.as_deref() {
        Some(path) => redirect_stderr(session_stderr(path)?),
        None => Ok(()),
    }
}

/// The `.stderr` file of a session on Windows. An older
/// file moves to `<file>.prev` first, so a relaunch keeps the errors of the
/// last daemon; if that move fails, the launch stops and the old file stays.
/// The folder of the file is made private before the file is created.
fn session_stderr(path: &Path) -> Result<File> {
    let base = crate::runtime_base();
    habilis_network::util::ensure_parent_private(Some(&base), path)
        .map_err(|error| anyhow::anyhow!("prepare {}: {error}", path.display()))?;
    let mut previous = path.as_os_str().to_owned();
    previous.push(".prev");
    match std::fs::rename(path, &previous) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => anyhow::bail!("move {} to .prev: {error}", path.display()),
    }
    File::create(path).map_err(|error| anyhow::anyhow!("create {}: {error}", path.display()))
}

/// Make `file` the stderr of this process, for the rest of its life. std reads
/// the handle of stderr from the process on each write, so `eprintln!` and the
/// log sink follow it.
fn redirect_stderr(file: File) -> Result<()> {
    use std::os::windows::io::{AsHandle as _, IntoRawHandle as _};

    // The stderr until now can be a pipe of the caller. A child of this
    // process, the detached daemon, must not inherit it.
    keep_from_child(std::io::stderr().as_handle());
    let handle = file.into_raw_handle();
    #[expect(unsafe_code, reason = "SetStdHandle has no safe std wrapper")]
    // SAFETY: `handle` is an open file handle that this process now owns and
    // never closes, so it stays valid as the stderr of the process.
    let set = unsafe { SetStdHandle(STD_ERROR_HANDLE, handle) };
    anyhow::ensure!(
        set != 0,
        "redirect stderr: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
