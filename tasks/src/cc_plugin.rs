use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::repo_root;

/// Marketplace and plugin identifiers for the `cc-plugin-*` tasks.
/// `agent-habilis-swarm` is a directory-source marketplace pointing at
/// this repo; `swarm` is the only plugin it exposes.
const CC_MARKETPLACE: &str = "agent-habilis-swarm";
const CC_PLUGIN: &str = "swarm@agent-habilis-swarm";

pub(crate) fn install(sh: &Shell) -> TaskOutcome {
    if !command_exists(sh, "claude") {
        return Err("`claude` CLI not found on PATH".into());
    }

    // Re-point the directory-source marketplace at THIS repo on every run.
    // A marketplace registered once survives the clone it pointed at: with a
    // second checkout on the machine, `marketplace update` would silently
    // refresh from the *other* tree and install stale skills. Removing
    // (best-effort) then adding the current root guarantees the install
    // reflects the repo `cargo task` is run from.
    let root = repo_root();
    eprintln!(
        "=> Pointing marketplace {CC_MARKETPLACE} at {}...",
        root.display()
    );
    let _ = cmd!(sh, "claude plugin marketplace remove {CC_MARKETPLACE}")
        .quiet()
        .run();
    cmd!(sh, "claude plugin marketplace add {root}")
        .quiet()
        .run()?;

    // No explicit plugin uninstall: removing the marketplace above already
    // dropped any prior install of this plugin, so `install` below starts
    // clean. (A separate uninstall would just error "not found".)
    eprintln!("=> Installing {CC_PLUGIN} from this repo...");
    cmd!(sh, "claude plugin install {CC_PLUGIN}")
        .quiet()
        .run()?;
    eprintln!("=> Reinstalled. A running Claude session still needs");
    eprintln!("   /reload-plugins; new `claude` invocations pick it up.");
    Ok(())
}

pub(crate) fn uninstall(sh: &Shell) -> TaskOutcome {
    if !command_exists(sh, "claude") {
        return Err("`claude` CLI not found on PATH".into());
    }

    // Both steps best-effort: the desired end state (plugin gone,
    // marketplace deregistered) is idempotent — a no-op when already
    // absent is success, not failure.
    eprintln!("=> Uninstalling {CC_PLUGIN} (if present)...");
    let _ = cmd!(sh, "claude plugin uninstall {CC_PLUGIN} -y")
        .quiet()
        .run();

    eprintln!("=> Removing marketplace {CC_MARKETPLACE} (if registered)...");
    let _ = cmd!(sh, "claude plugin marketplace remove {CC_MARKETPLACE}")
        .quiet()
        .run();

    eprintln!("=> Removed. A running Claude session still needs");
    eprintln!("   /reload-plugins to drop the skills.");
    Ok(())
}

fn command_exists(sh: &Shell, program: &str) -> bool {
    cmd!(sh, "{program} --version")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok()
}
