use xshell::{Shell, cmd};

use crate::Target;
use crate::TaskOutcome;
use crate::util::{
    SKILL_LINK_NAME, drop_legacy_marketplace, remove_existing, repo_root, resolve, skills_link,
    status, warn,
};

/// Set up the selected dev integrations (Claude Code plugin, pi extension).
/// Symmetric with [`crate::teardown::run`].
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
        &format!("set up {} integration(s)", selected.len()),
    );
    Ok(())
}

/// Install the Claude Code plugin: symlink the repo's `claude-code-plugin` into
/// the skills dir so it loads in place as `swarm@skills-dir`.
fn cc_plugin(sh: &Shell) -> TaskOutcome {
    let source = repo_root().join("claude-code-plugin");
    let link = skills_link()?;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Re-point on every run so the link always reflects the tree `cargo task`
    // is run from — a second checkout would otherwise keep loading the first.
    remove_existing(&link);
    std::os::unix::fs::symlink(&source, &link)?;
    status("Setting up", &format!("cc-plugin ({})", link.display()));

    drop_legacy_marketplace(sh);
    warn(&format!(
        "loads as `{SKILL_LINK_NAME}@skills-dir`; run /reload-plugins in a live Claude session"
    ));
    Ok(())
}

/// Install the pi extension from the repo's `pi-extension` source.
fn pi(sh: &Shell) -> TaskOutcome {
    let source = repo_root().join("pi-extension");
    status("Setting up", "pi extension");
    cmd!(sh, "pi install {source}").quiet().run()?;
    Ok(())
}
