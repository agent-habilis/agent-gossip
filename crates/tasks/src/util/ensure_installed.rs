use super::{Invocation, output};

/// Install `krate` via `cargo install --locked` if the probe command
/// (`check`) fails. Best-effort: a probe or install hiccup must not
/// abort the calling task — its own command surfaces a clear error if
/// the tool is genuinely missing.
pub(crate) fn ensure_installed(krate: &str, check: &[&str]) {
    let ok: bool = Invocation::cargo(check).silent().run().is_ok();

    if !ok {
        output::status("Installing", krate);
        let _ = Invocation::cargo(["install", "--locked", krate])
            .quiet()
            .run();
    }
}
