//! `ah-s setup` / `ah-s teardown`: install or remove the swarm integrations
//! across agents. Each artifact is embedded at compile time (like the manual),
//! so a brew/cargo-installed binary carries them with no repo or external
//! installer. Both commands are dry-run by default; `--execute` mutates.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use include_dir::Dir;

use crate::util::output::{status, status_warn, warn};

use super::agent::{Agent, CC_PLUGIN, GENERIC_SKILL, PI_EXTENSION, home_dir, skipped};

/// Which operation a default selection is for.
#[derive(Clone, Copy)]
enum Op {
    Install,
    Remove,
}

/// Install the embedded integrations into the selected agents. Dry run unless
/// `execute`.
///
/// # Errors
/// `$HOME` unset, a filesystem error, or `pi install` failing.
pub(crate) fn setup(execute: bool, agents: &[Agent]) -> Result<()> {
    let home = home_dir()?;
    let mut acted = 0;
    for agent in resolve(&home, agents, Op::Install) {
        if install(agent, &home, execute)? {
            acted += 1;
        }
    }
    finish(execute, acted, "set up");
    Ok(())
}

/// Remove the integrations from the selected agents — symmetric to [`setup`].
///
/// # Errors
/// `$HOME` unset, or a filesystem error while removing.
pub(crate) fn teardown(execute: bool, agents: &[Agent]) -> Result<()> {
    let home = home_dir()?;
    let mut acted = 0;
    for agent in resolve(&home, agents, Op::Remove) {
        if remove(agent, &home, execute)? {
            acted += 1;
        }
    }
    finish(execute, acted, "removed");
    Ok(())
}

/// Decide which agents to act on: explicit `--agent` flags, or — when none are
/// given — the default set for `op` (detected agents to install into, agents
/// that have it to remove from).
fn resolve(home: &Path, agents: &[Agent], op: Op) -> Vec<Agent> {
    if !agents.is_empty() {
        return dedup(agents);
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

/// `acted` counts agents actually installed/removed (not those skipped as
/// absent), so the summary never overstates what happened.
fn finish(execute: bool, acted: usize, verb: &str) {
    if acted == 0 {
        warn("nothing to do (try --agent claude-code|pi|generic)");
    } else if execute {
        status("Finished", &format!("{verb} swarm · {acted} agent(s)"));
    } else {
        warn("dry run; re-run with --execute to apply");
    }
}

/// Install the integration for one agent. `--execute` is authoritative: it
/// removes any existing install first, then writes fresh. Returns whether it
/// acted (always — every selected agent is installed); symmetric with [`remove`].
fn install(agent: Agent, home: &Path, execute: bool) -> Result<bool> {
    let path = agent.install_path(home);
    status(
        "Setting up",
        &format!("{} ({})", agent.label(), path.display()),
    );
    if !execute {
        return Ok(true);
    }
    match agent {
        Agent::ClaudeCode => {
            remove_existing(&path)?;
            write_dir(&CC_PLUGIN, &path)?;
        }
        Agent::Generic => {
            remove_existing(&path)?;
            let file = path.join("SKILL.md");
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating {}", path.display()))?;
            std::fs::write(&file, GENERIC_SKILL)
                .with_context(|| format!("writing {}", file.display()))?;
        }
        Agent::Pi => {
            remove_existing(&path)?;
            write_dir(&PI_EXTENSION, &path)?;
            pi(&["install", &path.to_string_lossy()])?;
        }
    }
    Ok(true)
}

/// Remove the integration for one agent — symmetric with [`install`]. Returns
/// whether it acted: `false` (a logged skip) when the agent has nothing
/// installed, so the caller's summary counts only real removals.
fn remove(agent: Agent, home: &Path, execute: bool) -> Result<bool> {
    let path = agent.install_path(home);
    if !agent.installed(home) {
        status_warn(
            "Skipping",
            &format!("{} (not present at {})", agent.label(), path.display()),
        );
        return Ok(false);
    }
    status(
        "Removing",
        &format!("{} ({})", agent.label(), path.display()),
    );
    if !execute {
        return Ok(true);
    }
    if agent == Agent::Pi {
        // Best-effort deregister before deleting the source it points at.
        let _ = pi(&["remove", &path.to_string_lossy()]);
    }
    remove_existing(&path)?;
    Ok(true)
}

/// Recursively write an embedded `Dir` to `dest`, skipping [`SKIP`] names.
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
fn remove_existing(path: &Path) -> Result<()> {
    if path.is_symlink() || path.is_file() {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    } else if path.is_dir() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

/// Run `pi <args>`, surfacing a clear error when `pi` is missing or fails.
/// Output is captured (not inherited) so pi's own chatter never breaks the
/// cargo-style status formatting; on failure its stderr is folded into the
/// error so the failure stays diagnosable.
fn pi(args: &[&str]) -> Result<()> {
    let output = Command::new("pi")
        .args(args)
        .output()
        .context("running `pi` (is the pi CLI on PATH?)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(":\n{detail}")
        };
        bail!(
            "`pi {}` failed with {}{suffix}",
            args.join(" "),
            output.status
        );
    }
    Ok(())
}
