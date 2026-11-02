use std::process::Child;
#[cfg(unix)]
use std::process::Command;

/// End a daemon that a test started, to clean up after the test.
///
/// SIGTERM on Unix, so it shuts down as on a stop, and on Windows
/// `Child::kill`, which ends it at once. A test that checks the shutdown itself
/// sends its own signal.
pub fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = child.kill();
    }
}
