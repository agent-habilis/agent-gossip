//! `auto` for the pid and path flags, on Windows only.
//!
//! The skills pass the agent process to `--owner-pid` and `--session-pid`,
//! and build the state file and the `.stderr` file from it. On Unix that is
//! the shell's `$PPID`. Under Git Bash on Windows `$PPID` is 1, because the
//! agent is not an MSYS process, and the shell cannot name the Windows pid.
//! So on Windows each of these flags takes `auto`, and the binary finds the
//! agent itself: the first ancestor of this process that is not a shell.
//! Every command of a skill runs under the same agent, so each one finds the
//! same pid, and the same files.
//!
//! The value is resolved while the arguments are parsed. The detached daemon
//! gets the resolved values from its launcher (`concrete_args`), because the
//! daemon's own parent is the launcher, not a shell.

use std::ffi::OsString;
use std::path::PathBuf;

use habilis_network::util::process;

/// The image names that `auto` walks past, compared without case. The shells
/// of Git Bash, MSYS2 and Cygwin, the shells of Windows, and this binary.
const PASSED: [&str; 14] = [
    "bash",
    "sh",
    "dash",
    "zsh",
    "ksh",
    "mksh",
    "fish",
    "tcsh",
    "csh",
    "busybox",
    "cmd",
    "powershell",
    "pwsh",
    "agent-gossip",
];

/// How far the walk goes up before it gives up.
const MAX_DEPTH: usize = 16;

/// The agent process that runs this command: the first ancestor that is not a
/// shell.
///
/// # Errors
/// A parent is missing or exited, its image name cannot be read, the walk reaches pid 0 or 4 (the idle
/// process and the system), or it passes [`MAX_DEPTH`] shells. It never
/// falls back to the last pid it reached.
pub(crate) fn agent_pid() -> Result<u32, String> {
    let mut current = std::process::id();
    for _ in 0..MAX_DEPTH {
        let parent = process::parent_of(current).ok_or_else(|| {
            format!("the parent of pid {current} is gone or cannot be queried, so the agent process is unknown")
        })?;
        if parent == 0 || parent == 4 {
            return Err(format!(
                "pid {current} has no agent above it (its parent is pid {parent})"
            ));
        }
        let name = process::comm_of(parent).ok_or_else(|| {
            format!(
                "the image name of pid {parent} cannot be read, so the agent process is unknown"
            )
        })?;
        if !PASSED
            .iter()
            .any(|passed| name.eq_ignore_ascii_case(passed))
        {
            return Ok(parent);
        }
        current = parent;
    }
    Err(format!(
        "more than {MAX_DEPTH} shells above this process, so the agent process is unknown"
    ))
}

/// The value parser of `--owner-pid` and `--session-pid` on Windows: a pid,
/// or `auto` for [`agent_pid`].
pub(crate) fn pid(value: &str) -> Result<u32, String> {
    if value == "auto" {
        agent_pid()
    } else {
        value.parse().map_err(|error| format!("{error}"))
    }
}

/// The folder of the session files of every agent: `<base>/sessions`.
fn sessions_dir() -> PathBuf {
    crate::runtime_base().join("sessions")
}

/// The value parser of `--state-file` on Windows: a path, or `auto` for
/// `<base>/sessions/<agent>.json`.
pub(crate) fn state_file(value: &str) -> Result<PathBuf, String> {
    if value == "auto" {
        Ok(sessions_dir().join(format!("{}.json", agent_pid()?)))
    } else {
        Ok(PathBuf::from(value))
    }
}

/// The value parser of `--stderr-file`: a path, or `auto` for
/// `<base>/sessions/<agent>.stderr`.
pub(crate) fn stderr_file(value: &str) -> Result<PathBuf, String> {
    if value == "auto" {
        Ok(sessions_dir().join(format!("{}.stderr", agent_pid()?)))
    } else {
        Ok(PathBuf::from(value))
    }
}

/// The `.stderr` file beside a state file: `<x>.json` gives `<x>.stderr`.
pub(crate) fn stderr_beside(state_file: &std::path::Path) -> PathBuf {
    state_file.with_extension("stderr")
}

/// The launcher's arguments for the detached daemon, with each `auto` replaced
/// by the value that the launcher resolved, and `--stderr-file` left out: the
/// launcher opens that file and hands it to the daemon as its stderr.
pub(crate) fn concrete_args(
    args: impl IntoIterator<Item = OsString>,
    owner_pid: u32,
    state_file: Option<&std::path::Path>,
) -> Vec<OsString> {
    let mut out = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        let (flag, inline) = match text.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (text.into_owned(), None),
        };
        let takes_value = matches!(
            flag.as_str(),
            "--owner-pid" | "--state-file" | "--stderr-file"
        );
        if !takes_value {
            out.push(arg);
            continue;
        }
        let value = match inline {
            Some(value) => OsString::from(value),
            None => args.next().unwrap_or_default(),
        };
        match flag.as_str() {
            "--owner-pid" => {
                out.push(flag.into());
                out.push(owner_pid.to_string().into());
            }
            "--state-file" => {
                out.push(flag.into());
                out.push(state_file.map_or(value, |path| path.as_os_str().to_owned()));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::concrete_args;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn concrete_args_replaces_auto_and_drops_the_stderr_file() {
        let state = std::path::Path::new(r"C:\base\sessions\42.json");
        let out = concrete_args(
            args(&[
                "create",
                "--name",
                "x",
                "--owner-pid",
                "auto",
                "--state-file=auto",
                "--stderr-file",
                "auto",
                "--quiet",
            ]),
            42,
            Some(state),
        );
        assert_eq!(
            out,
            args(&[
                "create",
                "--name",
                "x",
                "--owner-pid",
                "42",
                "--state-file",
                r"C:\base\sessions\42.json",
                "--quiet",
            ])
        );
    }

    #[test]
    fn agent_pid_finds_a_non_shell_ancestor() {
        // The test runner is no shell, so the walk stops at once or after the
        // shells that started it.
        let pid = super::agent_pid().expect("the test has an agent above it");
        assert_ne!(pid, std::process::id());
    }
}
