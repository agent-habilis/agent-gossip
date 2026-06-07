use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::output;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Installing", "ah-s");
    // `--force` is required: the crate version rarely changes between builds
    // (it stays `0.2.0` across many commits), and without `--force`
    // `cargo install --path .` treats "0.2.0 already installed" as up-to-date
    // and **skips the rebuild entirely**, silently leaving the previously
    // installed binary in place. That shipped a stale `ah-s` to fleet hosts
    // (the binary's git-hash stamp lagged the checked-out commit). `--force`
    // always rebuilds + reinstalls the current tree.
    cmd!(sh, "cargo install --path . --force").quiet().run()?;
    output::status("Installed", "~/.cargo/bin/ah-s");
    Ok(())
}
