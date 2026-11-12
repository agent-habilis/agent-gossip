use anyhow::Result;

use super::args::SharedServerOpts;
#[cfg(windows)]
use super::auto;
use super::detached_stdio::detached_stdio;
use super::spawn_detached::spawn_detached;

/// With `--owner-pid`, re-spawn this process as a daemon in its own session and
/// exit. The caller runs it after every argument is resolved, so an argument
/// error exits from the launcher, and before the mesh is set up, so no log
/// buffer is flushed into the caller's `.stderr` file.
pub(super) fn detach_if_owned(shared: &SharedServerOpts) -> Result<()> {
    let Some(owner) = shared.owner_pid else {
        return Ok(());
    };
    if shared.detached_child {
        // The launcher spawned this process without a new session. Take one before
        // anything else. Claude Code walks processes by session id, so a process group
        // alone is not enough. Failing here means a session is already led, which is fine.
        // On Windows the launcher already started it with no console and in its own
        // process group (`spawn_detached`).
        #[cfg(unix)]
        let _ = rustix::process::setsid();
        return Ok(());
    }
    habilis_network::runtime::validate_owner_pid(owner)?;
    let mut child = std::process::Command::new(std::env::current_exe()?);
    // On Windows `auto` was resolved from this launcher's parents, which the
    // daemon does not share, so the daemon gets the resolved values.
    #[cfg(unix)]
    child.args(std::env::args_os().skip(1));
    #[cfg(windows)]
    child.args(auto::concrete_args(
        std::env::args_os().skip(1),
        owner,
        shared.state_file.as_deref(),
    ));
    child
        .arg("--detached-child")
        .stdin(std::process::Stdio::null())
        .stdout(detached_stdio(&std::io::stdout()));
    #[cfg(unix)]
    child.stderr(detached_stdio(&std::io::stderr()));
    #[cfg(windows)]
    if shared.stderr_file.is_some() {
        // `stderr_to_session_file` already made the session file the
        // launcher's stderr, so the daemon inherits it.
        child.stderr(std::process::Stdio::inherit());
    } else {
        child.stderr(detached_stdio(&std::io::stderr()));
    }
    spawn_detached(&mut child)
        .map_err(|error| anyhow::anyhow!("failed to start the detached daemon: {error}"))?;
    std::process::exit(0)
}
