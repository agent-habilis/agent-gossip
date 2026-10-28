use xshell::{Shell, cmd};

use super::output;

/// Install `krate` via `cargo install --locked` if the probe command
/// (`check`) fails. Best-effort: a probe or install hiccup must not
/// abort the calling task — its own command surfaces a clear error if
/// the tool is genuinely missing.
pub(crate) fn ensure_installed(sh: &Shell, krate: &str, check: &[&str]) {
    let probe = cmd!(sh, "cargo {check...}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr();
    let ok: bool = probe.run().is_ok();

    if !ok {
        output::status("Installing", krate);
        let _ = cmd!(sh, "cargo install --locked {krate}").quiet().run();
    }
}
