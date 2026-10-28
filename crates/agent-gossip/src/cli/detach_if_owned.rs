use anyhow::Result;

use super::args::SharedServerOpts;
use super::detached_stdio::detached_stdio;

/// With `--owner-pid`, re-spawn this process as a daemon in its own session and
/// exit. The caller runs it after every argument is resolved, so an argument
/// error exits from the launcher, and before the mesh is set up, so no log
/// buffer is flushed into the caller's `.stderr` file.
pub(super) fn detach_if_owned(shared: &SharedServerOpts) -> Result<()> {
    let Some(owner) = shared.owner_pid else {
        return Ok(());
    };
    if shared.detached_child {
        return Ok(());
    }
    habilis_network::runtime::validate_owner_pid(owner)?;
    let mut child = std::process::Command::new(std::env::current_exe()?);
    child
        .args(std::env::args_os().skip(1))
        .arg("--detached-child")
        .stdin(std::process::Stdio::null())
        .stdout(detached_stdio(&std::io::stdout()))
        .stderr(detached_stdio(&std::io::stderr()));
    #[expect(
        unsafe_code,
        reason = "setsid has no safe std wrapper; process_group(0) is not enough because Claude Code also walks processes by session id"
    )]
    // SAFETY: `setsid` is async-signal-safe and touches no memory of this
    // process, so it is sound between fork and exec.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut child, || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    child
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to start the detached daemon: {error}"))?;
    std::process::exit(0)
}
