use std::process::Command;

#[cfg(windows)]
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
};

#[cfg(windows)]
use super::keep_from_child::keep_from_child;

/// Start `child`. On Unix the child takes its own session itself, first thing
/// (`detach_if_owned`).
#[cfg(unix)]
pub(super) fn spawn_detached(child: &mut Command) -> std::io::Result<()> {
    child.spawn().map(drop)
}

/// Start `child` with no console, in its own process group, and outside the
/// job of the launcher, so that closing the console or the job of the agent
/// does not end the daemon.
///
/// `CREATE_NO_WINDOW` is not set: Windows ignores it with `DETACHED_PROCESS`.
/// A process with no console that starts a console program gives it a new,
/// visible console, so a child of the daemon needs `CREATE_NO_WINDOW`. The
/// daemon starts no child today.
///
/// A job without `JOB_OBJECT_LIMIT_BREAKAWAY_OK` refuses
/// `CREATE_BREAKAWAY_FROM_JOB` with `ERROR_ACCESS_DENIED`. The launcher then
/// starts the daemon once more inside the job, and says so: the daemon ends
/// when the job ends.
///
/// std passes every inheritable handle of the launcher to the child, not only
/// the three it names. The stdin of the launcher, like a captured stream in
/// `detached_stdio`, is made non-inheritable here, so the daemon holds no end
/// of a pipe of the caller. The launcher exits right after the spawn.
#[cfg(windows)]
pub(super) fn spawn_detached(child: &mut Command) -> std::io::Result<()> {
    use std::os::windows::io::AsHandle as _;
    use std::os::windows::process::CommandExt as _;

    keep_from_child(std::io::stdin().as_handle());
    let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    child.creation_flags(detached | CREATE_BREAKAWAY_FROM_JOB);
    match child.spawn() {
        Ok(_) => Ok(()),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED.cast_signed()) => {
            child.creation_flags(detached);
            child.spawn()?;
            crate::status::warn(
                "the daemon could not leave the job of its parent, so it ends when the job ends",
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}
