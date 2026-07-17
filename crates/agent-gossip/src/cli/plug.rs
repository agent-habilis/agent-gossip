//! `agent-gossip plug` / `agent-gossip unplug`: install or remove the mesh integrations
//! across agents. Each artifact is embedded at compile time (like the manual),
//! so a brew/cargo-installed binary carries them with no repo or external
//! installer. Both act immediately; `plug` is reversible with `unplug`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::Dir;

use agent_habilis_mesh::util::output::{home_path, status, status_warn, warn};

use super::agent::{self, Agent, AgentState, SKILLS, home_dir, owned_skill_dirs_under, skipped};

/// Which operation a default selection is for.
#[derive(Clone, Copy)]
enum Op {
    Install,
    Remove,
}

/// Install the embedded integrations into the selected agents and paths.
///
/// # Errors
/// `$HOME` unset, or a filesystem error.
pub(crate) fn plug(agents: &[Agent], paths: &[PathBuf]) -> Result<()> {
    let home = home_dir()?;
    // Agent installs are silent — the roster below is the authoritative report.
    for agent in resolve(&home, agents, paths, Op::Install) {
        install(agent, &home)?;
    }
    for path in dedup_paths(paths) {
        install_root(&path)?;
        status("Installed", &format!("{} (path)", home_path(&path)));
    }
    // Always list every supported agent and where it landed.
    print_roster(&home);
    Ok(())
}

/// Remove the integrations from the selected agents and paths — symmetric to
/// [`plug`].
///
/// # Errors
/// `$HOME` unset, or a filesystem error while removing.
pub(crate) fn unplug(agents: &[Agent], paths: &[PathBuf]) -> Result<()> {
    let home = home_dir()?;
    let mut acted = 0;
    for agent in resolve(&home, agents, paths, Op::Remove) {
        if remove(agent, &home)? {
            acted += 1;
        }
    }
    for path in dedup_paths(paths) {
        if remove_root(&path)? {
            status("Unplugging", &format!("{} (path)", home_path(&path)));
            acted += 1;
        } else {
            status_warn(
                "Skipping",
                &format!("{} (nothing installed)", home_path(&path)),
            );
        }
    }
    finish(acted);
    Ok(())
}

/// Decide which agents to act on. Explicit `--agent` flags win. With no
/// `--agent` and no `--path`, fall back to the default set for `op` (detected
/// agents to install into, agents that have it to remove from). When only
/// `--path` is given, act on no agents — an explicit path is not a request to
/// touch every detected agent too.
fn resolve(home: &Path, agents: &[Agent], paths: &[PathBuf], op: Op) -> Vec<Agent> {
    if !agents.is_empty() {
        return dedup(agents);
    }
    if !paths.is_empty() {
        return Vec::new();
    }
    Agent::ALL
        .into_iter()
        .filter(|agent| match op {
            Op::Install => agent.detected(home),
            Op::Remove => agent.installed(home),
        })
        .collect()
}

/// `agents` with duplicates removed, preserving order.
fn dedup(agents: &[Agent]) -> Vec<Agent> {
    let mut out: Vec<Agent> = Vec::new();
    for &agent in agents {
        if !out.contains(&agent) {
            out.push(agent);
        }
    }
    out
}

/// `paths` with duplicates removed, preserving order.
fn dedup_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !out.contains(path) {
            out.push(path.clone());
        }
    }
    out
}

/// The `unplug` summary line — `plug` reports via [`print_roster`] instead.
fn finish(acted: usize) {
    if acted == 0 {
        warn("nothing to do (try --agent claude-code|pi|codex|cursor|opencode or --path DIR)");
    } else {
        let noun = if acted == 1 { "target" } else { "targets" };
        status("Finished", &format!("unplugging room · {acted} {noun}"));
    }
}

/// Install for one agent — silently; [`print_roster`] reports the outcome.
/// `plug` overwrites an existing agent's install but never creates config dirs
/// for an absent agent, even when selected explicitly with `--agent`.
fn install(agent: Agent, home: &Path) -> Result<()> {
    if !agent.detected(home) {
        return Ok(());
    }
    remove_owned(agent, home)?;
    write_dir(&SKILLS, &agent.install_path(home))?;
    Ok(())
}

/// Remove the integration for one agent — logs each removal for `unplug`.
/// Returns whether it acted (`false` = a logged skip), so the summary counts
/// only real removals.
fn remove(agent: Agent, home: &Path) -> Result<bool> {
    let path = agent.install_path(home);
    if !agent.installed(home) {
        status_warn(
            "Skipping",
            &format!("{} (not present at {})", agent.label(), path.display()),
        );
        return Ok(false);
    }
    status(
        "Unplugging",
        &format!("{} ({})", agent.label(), path.display()),
    );
    remove_owned(agent, home)?;
    Ok(true)
}

/// Install into an explicit directory as a skill root: clear the skill dirs
/// we own under it, then write fresh. No detection gate — the path is intent.
fn install_root(root: &Path) -> Result<()> {
    for dir in owned_skill_dirs_under(root) {
        remove_existing(&dir)?;
    }
    write_dir(&SKILLS, root)?;
    Ok(())
}

/// Remove the skill dirs we own under an explicit directory, leaving the folder
/// and anything else in it untouched. Returns whether anything was removed.
fn remove_root(root: &Path) -> Result<bool> {
    let mut removed = false;
    for dir in owned_skill_dirs_under(root) {
        removed |= remove_existing(&dir)?;
    }
    Ok(removed)
}

fn remove_owned(agent: Agent, home: &Path) -> Result<bool> {
    let mut removed = false;
    for path in agent.owned_skill_dirs(home) {
        removed |= remove_existing(&path)?;
    }
    Ok(removed)
}

/// List every supported agent and whether the integration is now installed for
/// it — the `plug` summary. Reads the same post-operation state `doctor` shows.
fn print_roster(home: &Path) {
    for (agent, path, state) in agent::states(home) {
        match state {
            AgentState::UpToDate => status(
                "Installed",
                &format!("{} ({})", agent.label(), home_path(&path)),
            ),
            AgentState::OutOfDate => status_warn(
                "Out of date",
                &format!("{} ({})", agent.label(), home_path(&path)),
            ),
            AgentState::NotSetUp => status_warn(
                "Skipped",
                &format!("{} (present, not installed)", agent.label()),
            ),
            AgentState::Absent => {
                status_warn("Skipped", &format!("{} (not detected)", agent.label()));
            }
        }
    }
}

/// Recursively write an embedded `Dir` to `dest`, skipping [`skipped`] names.
fn write_dir(dir: &Dir<'_>, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for file in dir.files() {
        if skipped(file.path()) {
            continue;
        }
        let target = dest.join(file.path().file_name().expect("embedded file has a name"));
        std::fs::write(&target, file.contents())
            .with_context(|| format!("writing {}", target.display()))?;
    }
    for sub in dir.dirs() {
        if skipped(sub.path()) {
            continue;
        }
        let name = sub.path().file_name().expect("embedded dir has a name");
        write_dir(sub, &dest.join(name))?;
    }
    Ok(())
}

/// Delete `path` if it exists, whatever it is. A symlink/file is unlinked
/// without touching its target; a real directory is removed recursively.
fn remove_existing(path: &Path) -> Result<bool> {
    if path.is_symlink() || path.is_file() {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        Ok(true)
    } else if path.is_dir() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{install_root, remove_root};

    /// `--path` install writes the skill tree into the directory as a root, and
    /// `--path` unplug removes only the skill dirs — never the folder or files
    /// the user keeps beside them.
    #[test]
    fn path_target_install_and_owned_only_removal() {
        let root = std::env::temp_dir().join(format!("agent-gossip-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // A file the user keeps in the same folder — must survive unplug.
        let sentinel = root.join("keep.txt");
        std::fs::write(&sentinel, "mine").unwrap();

        // A *directory* the user keeps beside the skills. `--path` can point at
        // any folder, so anything that is not an owned `gossip-*` dir belongs to
        // the user — including one named `shared/`, which the renderer never
        // emits (partials are inlined at build time).
        std::fs::create_dir_all(root.join("shared")).unwrap();
        std::fs::write(root.join("shared/old.md"), "mine too").unwrap();

        install_root(&root).unwrap();
        assert!(
            root.join("gossip-create/SKILL.md").is_file(),
            "install writes the per-command skills under the path"
        );
        assert!(
            !root.join("shared/SKILL.md").exists(),
            "install never renders shared/ as a skill"
        );

        let removed = remove_root(&root).unwrap();
        assert!(removed, "unplug reports it removed something");
        assert!(
            !root.join("gossip-create").exists(),
            "unplug removes the owned skill dirs"
        );
        assert!(sentinel.is_file(), "unplug leaves the user's own files");
        assert!(
            root.join("shared/old.md").is_file(),
            "unplug leaves unowned directories alone"
        );
        assert!(root.is_dir(), "unplug leaves the folder itself");

        // A second unplug is a no-op (nothing owned remains).
        assert!(!remove_root(&root).unwrap());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
