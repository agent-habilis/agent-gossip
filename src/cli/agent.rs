//! The agent/integration model shared by `plug`, `unplug`, and `doctor`:
//! the embedded skill tree, which agents exist, where each one's integration
//! lives, and whether it's set up / up to date.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};

/// The portable Agent Skills payload installed into each supported harness.
pub(crate) static SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/skills");

const OWNED_SKILL_DIRS: &[&str] = &[
    "square-create",
    "square-discover",
    "square-doctor",
    "square-handover",
    "square-join",
    "square-leave",
    "square-meta",
    "square-msg",
    "square-ping",
    "square-state",
    "square-status",
    "square-task",
    "square-topic",
    "shared",
];

/// Ties this module's compilation to the embedded artifacts' content
/// (fingerprint emitted by `build.rs`), so editing a skill file forces a
/// rebuild that re-expands the `include_dir!` embed above — `include_dir!` is
/// otherwise untracked on stable. Anonymous
/// `const _` so it's evaluated (the `env!` is the load-bearing part) but never
/// flagged as unused.
const _: &str = env!("AGENT_SQUARE_EMBED_FINGERPRINT");

/// Directory/file names never materialized — build cruft and local deps.
/// The exact same fragment `build.rs` uses to filter staging + the fingerprint,
/// so the embedded set, the written-out set, and the in-sync check can never
/// disagree.
const SKIP: &[&str] = include!("embed_skip.rs");

/// An agent the mesh integrations can be installed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Agent {
    /// Claude Code — skills under `~/.claude/skills`.
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    /// pi — skills under `~/.pi/agent/skills`.
    Pi,
    /// A generic agent following the `~/.agents/skills` convention.
    Generic,
    /// Codex — skills under `~/.codex/skills`.
    Codex,
    /// Cursor — skills under `~/.cursor/skills`.
    Cursor,
}

/// Whether the mesh integration is set up for an agent, as `doctor` reports.
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
    pub(crate) const ALL: [Agent; 5] = [
        Agent::ClaudeCode,
        Agent::Pi,
        Agent::Generic,
        Agent::Codex,
        Agent::Cursor,
    ];

    /// The agent's CLI label, for display.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Pi => "pi",
            Agent::Generic => "generic",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
        }
    }

    /// The agent's home dir — its presence is the detection signal that gates
    /// `plug` (default selection AND explicit `--agent`: no install into an
    /// absent agent).
    pub(crate) fn agent_dir(self, home: &Path) -> PathBuf {
        let part = match self {
            Agent::ClaudeCode => ".claude",
            Agent::Pi => ".pi",
            Agent::Generic => ".agents",
            Agent::Codex => ".codex",
            Agent::Cursor => ".cursor",
        };
        home.join(part)
    }

    pub(crate) fn detected(self, home: &Path) -> bool {
        self.agent_dir(home).exists()
    }

    /// The skill root this agent reads once installed.
    pub(crate) fn install_path(self, home: &Path) -> PathBuf {
        match self {
            Agent::ClaudeCode => home.join(".claude/skills"),
            Agent::Pi => home.join(".pi/agent/skills"),
            Agent::Generic => home.join(".agents/skills"),
            Agent::Codex => home.join(".codex/skills"),
            Agent::Cursor => home.join(".cursor/skills"),
        }
    }

    pub(crate) fn installed(self, home: &Path) -> bool {
        self.owned_skill_dirs(home)
            .into_iter()
            .chain(self.legacy_install_paths(home))
            .any(|path| path.is_symlink() || path.exists())
    }

    /// Does the installed copy match the embedded one?
    fn in_sync(self, home: &Path) -> bool {
        dir_in_sync(&SKILLS, &self.install_path(home))
            && self
                .legacy_install_paths(home)
                .into_iter()
                .all(|path| !path.exists() && !path.is_symlink())
    }

    pub(crate) fn owned_skill_dirs(self, home: &Path) -> Vec<PathBuf> {
        let root = self.install_path(home);
        OWNED_SKILL_DIRS
            .iter()
            .map(|name| root.join(name))
            .collect()
    }

    pub(crate) fn legacy_install_paths(self, home: &Path) -> Vec<PathBuf> {
        match self {
            Agent::ClaudeCode => vec![home.join(".claude/skills/square")],
            Agent::Pi => vec![home.join(".agent-square/pi-extension")],
            Agent::Generic => vec![home.join(".agents/skills/square")],
            Agent::Codex => Vec::new(),
            Agent::Cursor => vec![home.join(".cursor/skills/square")],
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
/// `agent-square doctor`'s Integrations section.
pub(crate) fn states(home: &Path) -> Vec<(Agent, PathBuf, AgentState)> {
    Agent::ALL
        .into_iter()
        .map(|agent| (agent, agent.install_path(home), agent.state(home)))
        .collect()
}

/// The one canonical "skill out of date" nag, shared by the `ready`-event
/// drift warning (below) and the MCP `square_version` tool — one source of
/// truth so the two can't drift apart. `agent-square plug` refreshes every
/// installed integration, so the message names no specific one.
pub(crate) const SKILL_DRIFT_MSG: &str =
    "⚠️ square skill out of date. Run `agent-square plug` to update";

/// A one-line drift warning if any installed integration has fallen behind the
/// binary (`OutOfDate`), else `None`. The daemon folds this into its `ready`
/// event so a stale skill nags the agent at mesh start; `agent-square doctor` is the
/// on-demand counterpart.
pub(crate) fn drift_warning(home: &Path) -> Option<String> {
    let any_stale = states(home)
        .into_iter()
        .any(|(_, _, state)| state == AgentState::OutOfDate);
    any_stale.then(|| SKILL_DRIFT_MSG.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Agent, OWNED_SKILL_DIRS, SKILLS};
    use include_dir::Dir;
    use std::path::Path;

    #[test]
    fn embedded_artifacts_carry_their_entrypoints() {
        for skill in OWNED_SKILL_DIRS {
            if *skill == "shared" {
                continue;
            }
            assert!(
                SKILLS.get_file(&format!("{skill}/SKILL.md")).is_some(),
                "{skill} entrypoint"
            );
        }
        assert!(SKILLS.get_dir("shared").is_some());
        assert!(SKILLS.get_file("shared/SKILL.md").is_none());
        assert!(SKILLS.get_dir("square").is_none());
    }

    #[test]
    fn portable_square_skill_layout_is_progressive() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
        for skill in OWNED_SKILL_DIRS {
            if *skill == "shared" {
                continue;
            }
            let skill_dir = root.join(skill);
            assert!(skill_dir.join("SKILL.md").is_file(), "{skill} entrypoint");
            assert!(skill_dir.join("workflow.md").is_file(), "{skill} workflow");
        }
        for skill in [
            "square-create",
            "square-discover",
            "square-join",
            "square-topic",
        ] {
            let skill_dir = root.join(skill);
            assert!(
                skill_dir.join("adapters/generic.md").is_file(),
                "{skill} daemon adapter"
            );
        }
        // `create`/`join`/`topic` have exactly one adapter: every harness starts
        // the daemon and polls the same way. A Claude-Code-specific adapter is
        // what carried the Monitor event path, which truncated message bodies at
        // 500 chars and persisted them to disk. `discover` still needs Monitor —
        // it streams until killed and has no poll equivalent.
        for skill in ["square-create", "square-join", "square-topic"] {
            assert!(
                !root.join(skill).join("adapters/claude-code.md").exists(),
                "{skill} must not reintroduce a Monitor adapter"
            );
        }
        assert!(
            root.join("square-discover/adapters/claude-code.md")
                .is_file(),
            "square-discover still has a Monitor adapter (tracked follow-up)"
        );
        assert!(root.join("shared/harness-detect.md").is_file());
        assert!(root.join("shared/receive-loop.md").is_file());
        assert!(root.join("shared/events.md").is_file());
        assert!(root.join("shared/meta.md").is_file());
        assert!(root.join("shared/reattach.md").is_file());
        assert!(!root.join("shared/SKILL.md").exists());
    }

    /// A harness writes a background command's output to a file. The daemon's
    /// `--output json` stdout carries every message body, and its stderr prints
    /// the bare square id — a join credential (`Output::mesh_id_line`). The
    /// `> /dev/null 2>&1` in each adapter is therefore the only thing keeping
    /// either off disk; dropping one would reintroduce the leak silently, so pin
    /// it here rather than trust review.
    ///
    /// Stated as a property of *any* backgrounded `agent-square` line, so a
    /// future adapter is covered too.
    #[test]
    fn backgrounded_square_commands_discard_stdout_and_stderr() {
        for skill in ["square-create", "square-join", "square-topic"] {
            let path = format!("{skill}/adapters/generic.md");
            let adapter = SKILLS
                .get_file(&path)
                .and_then(include_dir::File::contents_utf8)
                .unwrap_or_else(|| panic!("{path} is embedded"));

            let backgrounded: Vec<&str> = adapter
                .lines()
                .map(str::trim_end)
                .filter(|line| line.starts_with("agent-square ") && line.ends_with('&'))
                .collect();
            assert!(
                backgrounded.len() >= 2,
                "{path}: expected the daemon launch and the poll bell to be backgrounded, found {backgrounded:?}"
            );
            for line in backgrounded {
                assert!(
                    line.contains("> /dev/null 2>&1"),
                    "{path}: a backgrounded command must discard stdout AND stderr, \
                     or the harness writes message bodies and the square id to a \
                     file: {line}"
                );
            }
        }
    }

    #[test]
    fn install_paths_are_under_home() {
        let home = Path::new("/home/x");
        assert!(Agent::ClaudeCode
            .install_path(home)
            .ends_with(".claude/skills"));
        assert!(Agent::Pi.install_path(home).ends_with(".pi/agent/skills"));
        assert!(Agent::Generic
            .install_path(home)
            .ends_with(".agents/skills"));
        assert!(Agent::Codex.install_path(home).ends_with(".codex/skills"));
        assert!(Agent::Cursor.install_path(home).ends_with(".cursor/skills"));
    }

    #[test]
    fn generic_in_sync_only_when_skill_tree_matches_embedded() {
        let home = std::env::temp_dir().join(format!("agent-square-insync-{}", std::process::id()));
        let dir = Agent::Generic.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!Agent::Generic.in_sync(&home));

        write_embedded_dir(&SKILLS, &dir);
        assert!(Agent::Generic.in_sync(&home));

        let file = dir.join("square-create/SKILL.md");
        let mut contents = std::fs::read_to_string(&file).unwrap();
        contents.push('\n');
        std::fs::write(&file, contents).unwrap();
        assert!(!Agent::Generic.in_sync(&home));

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn cursor_in_sync_only_when_skill_tree_matches_embedded() {
        let home =
            std::env::temp_dir().join(format!("agent-square-cursor-insync-{}", std::process::id()));
        let dir = Agent::Cursor.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!Agent::Cursor.in_sync(&home));

        write_embedded_dir(&SKILLS, &dir);
        assert!(Agent::Cursor.in_sync(&home));

        std::fs::write(dir.join("shared/extra.md"), "extra").unwrap();
        assert!(Agent::Cursor.in_sync(&home));
        std::fs::remove_file(dir.join("square-join/workflow.md")).unwrap();
        assert!(!Agent::Cursor.in_sync(&home));

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn drift_warning_fires_only_for_a_diverged_install() {
        let home = std::env::temp_dir().join(format!("agent-square-drift-{}", std::process::id()));
        let dir = Agent::Generic.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();

        write_embedded_dir(&SKILLS, &dir);
        assert!(super::drift_warning(&home).is_none());

        std::fs::write(dir.join("square-join/SKILL.md"), "stale").unwrap();
        let warning = super::drift_warning(&home).expect("diverged install warns");
        assert_eq!(warning, super::SKILL_DRIFT_MSG);
        assert!(warning.contains("out of date"));
        assert!(warning.contains("agent-square plug"));

        std::fs::remove_dir_all(&home).unwrap();
    }

    fn write_embedded_dir(dir: &Dir<'_>, dest: &Path) {
        std::fs::create_dir_all(dest).unwrap();
        for file in dir.files() {
            let target = dest.join(file.path().file_name().expect("embedded file has a name"));
            std::fs::write(target, file.contents()).unwrap();
        }
        for subdir in dir.dirs() {
            let name = subdir.path().file_name().expect("embedded dir has a name");
            write_embedded_dir(subdir, &dest.join(name));
        }
    }
}
