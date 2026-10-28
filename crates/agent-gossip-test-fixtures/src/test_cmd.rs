use std::process::Command;

use crate::{bin, test_log_dir};

/// `Command` for the built binary with `--log-dir` redirected to a
/// per-test temp dir. Use instead of `Command::new(bin())`. (`--log-dir`
/// is a global flag, so it sits before the subcommand here.)
pub fn test_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.arg("--log-dir").arg(test_log_dir());
    cmd
}
