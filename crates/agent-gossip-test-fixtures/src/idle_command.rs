use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;

/// A process that does nothing for `secs` seconds, to stand in as an owner or a parent.
///
/// `sleep` on Unix, and on Windows `ping` of the loopback, one second a ping.
#[must_use]
pub fn idle_command(secs: u32) -> Command {
    #[cfg(unix)]
    {
        let mut command = Command::new("sleep");
        command.arg(secs.to_string());
        command
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("ping");
        command
            .args(["-n", &(secs + 1).to_string(), "127.0.0.1"])
            .stdout(Stdio::null());
        command
    }
}
