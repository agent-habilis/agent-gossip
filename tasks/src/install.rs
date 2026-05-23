use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Installing ahs...");
    cmd!(sh, "cargo install --path .").quiet().run()?;
    eprintln!("=> Installed to ~/.cargo/bin/ahs");
    Ok(())
}
