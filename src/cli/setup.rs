//! `ah-s setup` / `ah-s teardown`: install or remove the swarm integrations
//! across agents. Each artifact is embedded at compile time (like the manual),
//! so a brew/cargo-installed binary carries them with no repo or external
//! installer. Both commands are dry-run by default; `--execute` mutates.

use std::path::{Path, PathBuf};
use std::process::Command;

use anstyle::{AnsiColor, Style};
use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};

/// Print a cargo-style status line: a right-aligned (12-col), bold-green `verb`
/// then `msg`. Via `anstream`, which strips color when stderr isn't a terminal
/// (so piped/agent output stays clean). Mirrors `../browse`'s `setup` output.
fn status(verb: &str, msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Green.into())).bold();
    anstream::eprintln!("{style}{verb:>12}{style:#} {msg}");
}

/// Print a cargo-style `warning: {msg}` line (bold-yellow `warning`).
fn warn(msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
    anstream::eprintln!("{style}warning{style:#}: {msg}");
}

/// The Claude Code plugin — multi-skill, loads as `swarm@skills-dir`.
static CC_PLUGIN: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/claude-code-plugin");
/// The pi extension — TS source (peer deps come from the pi runtime). Embedded
/// from the `build.rs`-staged copy in `OUT_DIR`, which excludes the local
/// `node_modules`, so it never bloats the binary.
static PI_EXTENSION: Dir<'_> = include_dir!("$OUT_DIR/pi-extension");
/// The portable, agent-agnostic MCP skill.
const GENERIC_SKILL: &str = include_str!("../../skills/swarm/SKILL.md");

/// Ties this module's compilation to the embedded artifacts' content
/// (fingerprint emitted by `build.rs`), so editing a plugin/skill/extension
/// file forces a rebuild that re-expands the `include_dir!`/`include_str!`
/// embeds above — `include_dir!` is otherwise untracked on stable. Anonymous
/// `const _` so it's evaluated (the `env!` is the load-bearing part) but never
/// flagged as unused.
const _: &str = env!("AHS_EMBED_FINGERPRINT");

/// Directory/file names never materialized — build cruft and pi's local deps.
/// The exact same fragment `build.rs` uses to filter staging + the fingerprint,
/// so the embedded set and the written-out set can never disagree.
const SKIP: &[&str] = include!("embed_skip.rs");

/// An agent the swarm integrations can be installed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Agent {
    /// Claude Code — the plugin at `~/.claude/skills/swarm`.
    #[value(name = "claude", alias = "claude-code")]
    ClaudeCode,
    /// pi — the extension installed via `pi install`.
    Pi,
    /// A generic agent following the `~/.agents/skills` convention.
    Generic,
}

impl Agent {
    /// Every agent, in display order.
    const ALL: [Agent; 3] = [Agent::ClaudeCode, Agent::Pi, Agent::Generic];

    fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude",
            Agent::Pi => "pi",
            Agent::Generic => "generic",
        }
    }

    /// The agent's home dir (`~/.claude`, `~/.pi`, `~/.agents`) — its presence
    /// is the detection signal that seeds the default selection.
    fn agent_dir(self, home: &Path) -> PathBuf {
        let part = match self {
            Agent::ClaudeCode => ".claude",
            Agent::Pi => ".pi",
            Agent::Generic => ".agents",
        };
        home.join(part)
    }

    fn detected(self, home: &Path) -> bool {
        self.agent_dir(home).exists()
    }

    /// The path this agent's integration lives at once installed.
    fn install_path(self, home: &Path) -> PathBuf {
        match self {
            Agent::ClaudeCode => home.join(".claude/skills/swarm"),
            // pi-package source, materialized then `pi install`ed.
            Agent::Pi => home.join(".agent-habilis/swarm/pi-extension"),
            Agent::Generic => home.join(".agents/skills/swarm"),
        }
    }

    fn installed(self, home: &Path) -> bool {
        let path = self.install_path(home);
        path.is_symlink() || path.exists()
    }
}

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

fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("`$HOME` is not set")?;
    Ok(PathBuf::from(home))
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
        warn("nothing to do (try --agent claude|pi|generic)");
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
        status(
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

/// Is this embedded path's final component in the skip list?
fn skipped(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SKIP.contains(&name))
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
fn pi(args: &[&str]) -> Result<()> {
    let status = Command::new("pi")
        .args(args)
        .status()
        .context("running `pi` (is the pi CLI on PATH?)")?;
    if !status.success() {
        bail!("`pi {}` failed with {status}", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Agent, CC_PLUGIN, GENERIC_SKILL, PI_EXTENSION};
    use std::path::Path;

    #[test]
    fn embedded_artifacts_carry_their_entrypoints() {
        // include_dir paths are relative to the embedded root (no dir-name prefix).
        assert!(CC_PLUGIN.get_file(".claude-plugin/plugin.json").is_some());
        assert!(PI_EXTENSION.get_file("index.ts").is_some());
        assert!(GENERIC_SKILL.starts_with("---"));
        assert!(GENERIC_SKILL.contains("name: swarm"));
    }

    #[test]
    fn install_paths_are_under_home() {
        let home = Path::new("/home/x");
        assert!(
            Agent::ClaudeCode
                .install_path(home)
                .ends_with(".claude/skills/swarm")
        );
        assert!(
            Agent::Pi
                .install_path(home)
                .ends_with(".agent-habilis/swarm/pi-extension")
        );
        assert!(
            Agent::Generic
                .install_path(home)
                .ends_with(".agents/skills/swarm")
        );
    }
}
