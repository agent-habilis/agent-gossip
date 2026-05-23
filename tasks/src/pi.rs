use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::repo_root;

pub(crate) fn link(sh: &Shell) -> TaskOutcome {
    let src = repo_root().join("pi-extension");
    eprintln!("=> Installing pi extension...");
    cmd!(sh, "pi install {src}").quiet().run()?;
    Ok(())
}

pub(crate) fn unlink(sh: &Shell) -> TaskOutcome {
    let src = repo_root().join("pi-extension");
    eprintln!("=> Removing pi extension...");
    cmd!(sh, "pi remove {src}").quiet().run()?;
    Ok(())
}

pub(crate) fn typecheck(sh: &Shell) -> TaskOutcome {
    ensure_pi_deps(sh)?;
    eprintln!("=> Type-checking pi-extension...");
    cmd!(sh, "npm --prefix pi-extension run typecheck")
        .quiet()
        .run()?;
    Ok(())
}

pub(crate) fn lint(sh: &Shell) -> TaskOutcome {
    ensure_pi_deps(sh)?;
    eprintln!("=> Linting pi-extension...");
    cmd!(sh, "npm --prefix pi-extension run lint")
        .quiet()
        .run()?;
    Ok(())
}

fn ensure_pi_deps(sh: &Shell) -> TaskOutcome {
    if pi_deps_are_fresh() {
        return Ok(());
    }
    if std::path::Path::new("pi-extension/package-lock.json").exists() {
        eprintln!("=> Installing pi-extension deps (npm ci)...");
        cmd!(sh, "npm --prefix pi-extension ci").quiet().run()?;
    } else {
        eprintln!("=> Installing pi-extension deps (npm install)...");
        cmd!(sh, "npm --prefix pi-extension install")
            .quiet()
            .run()?;
    }
    Ok(())
}

fn pi_deps_are_fresh() -> bool {
    // npm rewrites this marker on every install — newer than package.json means up-to-date.
    let marker = std::path::Path::new("pi-extension/node_modules/.package-lock.json");
    let pkg = std::path::Path::new("pi-extension/package.json");
    let Ok(marker_mtime) = marker.metadata().and_then(|meta| meta.modified()) else {
        return false;
    };
    let Ok(pkg_mtime) = pkg.metadata().and_then(|meta| meta.modified()) else {
        return false;
    };
    marker_mtime >= pkg_mtime
}
