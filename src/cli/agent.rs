//! The agent/integration model shared by `plug`, `unplug`, and `doctor`:
//! the artifacts embedded in this binary, which agents exist, where each one's
//! integration lives, and whether it's set up / up to date. Mirrors
//! `../browse`'s `util::skill` (embed + agent + state co-located).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};

/// The Claude Code plugin — multi-skill, loads as `gossip@skills-dir`.
pub(crate) static CC_PLUGIN: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/claude-code-plugin");
/// The pi extension — TS source (peer deps come from the pi runtime). Embedded
/// from the `build.rs`-staged copy in `OUT_DIR`, which excludes the local
/// `node_modules`, so it never bloats the binary.
pub(crate) static PI_EXTENSION: Dir<'_> = include_dir!("$OUT_DIR/pi-extension");
/// The portable, agent-agnostic MCP skill.
pub(crate) const GENERIC_SKILL: &str = include_str!("../../skills/gossip/SKILL.md");

/// Ties this module's compilation to the embedded artifacts' content
/// (fingerprint emitted by `build.rs`), so editing a plugin/skill/extension
/// file forces a rebuild that re-expands the `include_dir!`/`include_str!`
/// embeds above — `include_dir!` is otherwise untracked on stable. Anonymous
/// `const _` so it's evaluated (the `env!` is the load-bearing part) but never
/// flagged as unused.
const _: &str = env!("AGENT_GOSSIP_EMBED_FINGERPRINT");

/// Directory/file names never materialized — build cruft and pi's local deps.
/// The exact same fragment `build.rs` uses to filter staging + the fingerprint,
/// so the embedded set, the written-out set, and the in-sync check can never
/// disagree.
const SKIP: &[&str] = include!("embed_skip.rs");

/// An agent the swarm integrations can be installed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Agent {
    /// Claude Code — the plugin at `~/.claude/skills/gossip`.
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    /// pi — the extension installed via `pi install`.
    Pi,
    /// A generic agent following the `~/.agents/skills` convention.
    Generic,
    /// Cursor — the skill at `~/.cursor/skills/gossip`.
    Cursor,
}

/// Whether the swarm integration is set up for an agent, as `doctor` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    /// Set up, and the installed copy matches the one embedded in this binary.
    UpToDate,
    /// Set up, but the installed copy differs from the embedded one — the
    /// binary was upgraded past the install. Re-run `plug` to refresh.
    OutOfDate,
    /// The agent is present on this machine, but the integration isn't set up.
    NotSetUp,
    /// The agent isn't present on this machine.
    Absent,
}

impl AgentState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            AgentState::UpToDate => "up to date",
            AgentState::OutOfDate => "out of date",
            AgentState::NotSetUp => "not set up",
            AgentState::Absent => "absent",
        }
    }
}

impl Agent {
    /// Every agent, in display order.
    pub(crate) const ALL: [Agent; 4] =
        [Agent::ClaudeCode, Agent::Pi, Agent::Generic, Agent::Cursor];

    /// The agent's CLI label (`claude` / `pi` / `generic` / `cursor`), for display.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Pi => "pi",
            Agent::Generic => "generic",
            Agent::Cursor => "cursor",
        }
    }

    /// The agent's home dir (`~/.claude`, `~/.pi`, `~/.agents`, `~/.cursor`) —
    /// its presence is the detection signal that gates `plug` (default
    /// selection AND explicit `--agent`: no install into an absent agent).
    pub(crate) fn agent_dir(self, home: &Path) -> PathBuf {
        let part = match self {
            Agent::ClaudeCode => ".claude",
            Agent::Pi => ".pi",
            Agent::Generic => ".agents",
            Agent::Cursor => ".cursor",
        };
        home.join(part)
    }

    pub(crate) fn detected(self, home: &Path) -> bool {
        self.agent_dir(home).exists()
    }

    /// The path this agent's integration lives at once installed.
    pub(crate) fn install_path(self, home: &Path) -> PathBuf {
        match self {
            Agent::ClaudeCode => home.join(".claude/skills/gossip"),
            // pi-package source, materialized then `pi install`ed.
            Agent::Pi => home.join(".agent-gossip/pi-extension"),
            Agent::Generic => home.join(".agents/skills/gossip"),
            // Cursor reads global Agent Skills from `~/.cursor/skills`; it
            // gets the same portable skill the generic target ships.
            Agent::Cursor => home.join(".cursor/skills/gossip"),
        }
    }

    pub(crate) fn installed(self, home: &Path) -> bool {
        let path = self.install_path(home);
        path.is_symlink() || path.exists()
    }

    /// Does the installed copy match the embedded one?
    fn in_sync(self, home: &Path) -> bool {
        let path = self.install_path(home);
        match self {
            Agent::ClaudeCode => dir_in_sync(&CC_PLUGIN, &path),
            Agent::Pi => dir_in_sync(&PI_EXTENSION, &path),
            Agent::Generic | Agent::Cursor => std::fs::read(path.join("SKILL.md"))
                .is_ok_and(|on_disk| on_disk == GENERIC_SKILL.as_bytes()),
        }
    }

    /// up to date / out of date / not set up / absent for this agent.
    pub(crate) fn state(self, home: &Path) -> AgentState {
        if self.installed(home) {
            if self.in_sync(home) {
                AgentState::UpToDate
            } else {
                AgentState::OutOfDate
            }
        } else if self.detected(home) {
            AgentState::NotSetUp
        } else {
            AgentState::Absent
        }
    }
}

/// Is this embedded path's final component in the skip list? Shared by the
/// `plug` writer and the `in_sync` comparison so both filter identically.
pub(crate) fn skipped(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SKIP.contains(&name))
}

/// True iff every embedded file under `dir` (skipping [`SKIP`], recursing
/// subdirs) exists on disk at `dest` with identical bytes. Extra on-disk files
/// are ignored — only what `plug` would have written must match.
fn dir_in_sync(dir: &Dir<'_>, dest: &Path) -> bool {
    for file in dir.files() {
        if skipped(file.path()) {
            continue;
        }
        let target = dest.join(file.path().file_name().expect("embedded file has a name"));
        if std::fs::read(&target).is_ok_and(|on_disk| on_disk == file.contents()) {
            continue;
        }
        return false;
    }
    for sub in dir.dirs() {
        if skipped(sub.path()) {
            continue;
        }
        let name = sub.path().file_name().expect("embedded dir has a name");
        if !dir_in_sync(sub, &dest.join(name)) {
            return false;
        }
    }
    true
}

/// `$HOME` as a path.
///
/// # Errors
/// `$HOME` is unset.
pub(crate) fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("`$HOME` is not set")?;
    Ok(PathBuf::from(home))
}

/// Each agent, its install path, and its state — in display order. Drives
/// `agent-gossip doctor`'s Integrations section.
pub(crate) fn states(home: &Path) -> Vec<(Agent, PathBuf, AgentState)> {
    Agent::ALL
        .into_iter()
        .map(|agent| (agent, agent.install_path(home), agent.state(home)))
        .collect()
}

/// The one canonical "skill out of date" nag, shared by the `ready`-event
/// drift warning (below) and the MCP `swarm_version` tool — one source of
/// truth so the two can't drift apart. `agent-gossip plug` refreshes every
/// installed integration, so the message names no specific one.
pub(crate) const SKILL_DRIFT_MSG: &str =
    "⚠️ swarm skill out of date. Run `agent-gossip plug` to update";

/// A one-line drift warning if any installed integration has fallen behind the
/// binary (`OutOfDate`), else `None`. The daemon folds this into its `ready`
/// event so a stale skill nags the agent at swarm start; `agent-gossip doctor` is the
/// on-demand counterpart.
pub(crate) fn drift_warning(home: &Path) -> Option<String> {
    let any_stale = states(home)
        .into_iter()
        .any(|(_, _, state)| state == AgentState::OutOfDate);
    any_stale.then(|| SKILL_DRIFT_MSG.to_owned())
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
        assert!(GENERIC_SKILL.contains("name: gossip"));
    }

    #[test]
    fn install_paths_are_under_home() {
        let home = Path::new("/home/x");
        assert!(
            Agent::ClaudeCode
                .install_path(home)
                .ends_with(".claude/skills/gossip")
        );
        assert!(
            Agent::Pi
                .install_path(home)
                .ends_with(".agent-gossip/pi-extension")
        );
        assert!(
            Agent::Generic
                .install_path(home)
                .ends_with(".agents/skills/gossip")
        );
        assert!(
            Agent::Cursor
                .install_path(home)
                .ends_with(".cursor/skills/gossip")
        );
    }

    #[test]
    fn generic_in_sync_only_when_skill_matches_embedded() {
        let home = std::env::temp_dir().join(format!("agent-gossip-insync-{}", std::process::id()));
        let dir = Agent::Generic.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();

        // No SKILL.md yet → out of sync.
        assert!(!Agent::Generic.in_sync(&home));

        let file = dir.join("SKILL.md");
        std::fs::write(&file, GENERIC_SKILL).unwrap();
        assert!(Agent::Generic.in_sync(&home));

        // A diverged copy → out of sync.
        std::fs::write(&file, format!("{GENERIC_SKILL}\n")).unwrap();
        assert!(!Agent::Generic.in_sync(&home));

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn cursor_in_sync_only_when_skill_matches_embedded() {
        let home =
            std::env::temp_dir().join(format!("agent-gossip-cursor-insync-{}", std::process::id()));
        let dir = Agent::Cursor.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();

        // No SKILL.md yet → out of sync.
        assert!(!Agent::Cursor.in_sync(&home));

        // Cursor carries the same portable skill the generic target ships.
        let file = dir.join("SKILL.md");
        std::fs::write(&file, GENERIC_SKILL).unwrap();
        assert!(Agent::Cursor.in_sync(&home));

        // A diverged copy → out of sync.
        std::fs::write(&file, format!("{GENERIC_SKILL}\n")).unwrap();
        assert!(!Agent::Cursor.in_sync(&home));

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn drift_warning_fires_only_for_a_diverged_install() {
        let home = std::env::temp_dir().join(format!("agent-gossip-drift-{}", std::process::id()));
        let dir = Agent::Generic.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("SKILL.md");

        // Installed and matching → no warning (claude-code/pi/cursor are absent).
        std::fs::write(&file, GENERIC_SKILL).unwrap();
        assert!(super::drift_warning(&home).is_none());

        // Diverged install → the canonical drift warning.
        std::fs::write(&file, format!("{GENERIC_SKILL}\n")).unwrap();
        let warning = super::drift_warning(&home).expect("diverged install warns");
        assert_eq!(warning, super::SKILL_DRIFT_MSG);
        assert!(warning.contains("out of date"));
        assert!(warning.contains("agent-gossip plug"));

        std::fs::remove_dir_all(&home).unwrap();
    }
}
