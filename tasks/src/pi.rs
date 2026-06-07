//! pi-extension dev checks (`pi-typecheck` / `pi-lint`, also run by `ci`).
//! Install/uninstall of the extension lives in `setup`/`teardown`.

use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn typecheck(sh: &Shell) -> TaskOutcome {
    ensure_pi_deps(sh)?;
    eprintln!("=> Type-checking pi-extension...");
    let _guard = sh.push_dir("pi-extension");
    cmd!(sh, "bun run typecheck").quiet().run()?;
    Ok(())
}

pub(crate) fn lint(sh: &Shell) -> TaskOutcome {
    ensure_pi_deps(sh)?;
    eprintln!("=> Linting pi-extension...");
    let _guard = sh.push_dir("pi-extension");
    cmd!(sh, "bun run lint").quiet().run()?;
    Ok(())
}

fn ensure_pi_deps(sh: &Shell) -> TaskOutcome {
    if pi_deps_are_fresh() {
        return Ok(());
    }
    eprintln!("=> Installing pi-extension deps (bun install)...");
    let _guard = sh.push_dir("pi-extension");
    cmd!(sh, "bun install").quiet().run()?;
    Ok(())
}

fn pi_deps_are_fresh() -> bool {
    // `bun install` populates node_modules; it exists only after a successful
    // install, so a node_modules newer than package.json means deps are fresh.
    let marker = std::path::Path::new("pi-extension/node_modules");
    let pkg = std::path::Path::new("pi-extension/package.json");
    let Ok(marker_mtime) = marker.metadata().and_then(|meta| meta.modified()) else {
        return false;
    };
    let Ok(pkg_mtime) = pkg.metadata().and_then(|meta| meta.modified()) else {
        return false;
    };
    marker_mtime >= pkg_mtime
}
