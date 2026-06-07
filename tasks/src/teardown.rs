use xshell::{Shell, cmd};

use crate::Target;
use crate::TaskOutcome;
use crate::util::{
    drop_legacy_marketplace, remove_existing, repo_root, resolve, skills_link, status, warn,
};

/// Tear down the selected dev integrations (Claude Code plugin, pi extension).
/// Symmetric with [`crate::setup::run`].
pub(crate) fn run(sh: &Shell, targets: &[Target]) -> TaskOutcome {
    let selected = resolve(targets);
    for target in &selected {
        match target {
            Target::CcPlugin => cc_plugin(sh)?,
            Target::Pi => pi(sh)?,
        }
    }
    status(
        "Finished",
        &format!("removed {} integration(s)", selected.len()),
    );
    Ok(())
}

/// Remove the Claude Code plugin's skills-dir symlink.
fn cc_plugin(sh: &Shell) -> TaskOutcome {
    let link = skills_link()?;
    remove_existing(&link);
    status("Removing", &format!("cc-plugin ({})", link.display()));

    drop_legacy_marketplace(sh);
    warn("run /reload-plugins in a live Claude session to drop it");
    Ok(())
}

/// Remove the pi extension.
fn pi(sh: &Shell) -> TaskOutcome {
    let source = repo_root().join("pi-extension");
    status("Removing", "pi extension");
    cmd!(sh, "pi remove {source}").quiet().run()?;
    Ok(())
}
