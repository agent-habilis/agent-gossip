//! The agent/integration model shared by `plug`, `unplug`, and `doctor`:
//! the embedded skill tree, which agents exist, where each one's integration
//! lives, and whether it's set up / up to date.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};

/// The portable Agent Skills payload installed into each supported harness:
/// the single-file-per-skill tree `build.rs` renders from the multi-file
/// `skills/` sources into `$OUT_DIR/skills`.
pub(crate) static SKILLS: Dir<'_> = include_dir!("$OUT_DIR/skills");

/// Dirs plug/unplug own but that are no longer embedded — the pre-single-file
/// `shared/` partials and removed skills. Old installs left them on disk;
/// keeping them owned lets plug/unplug clean those up.
const RETIRED_SKILL_DIRS: &[&str] = &["shared", "square-handover"];

const OWNED_SKILL_DIRS: &[&str] = &[
    "square-create",
    "square-discover",
    "square-doctor",
    "square-join",
    "square-leave",
    "square-meta",
    "square-msg",
    "square-ping",
    "square-review",
    "square-state",
    "square-status",
    "square-task",
    "square-topic",
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
    /// Codex — skills under `~/.codex/skills`.
    Codex,
    /// Cursor — skills under `~/.cursor/skills`.
    Cursor,
    /// opencode — skills under `~/.config/opencode/skills`.
    #[value(name = "opencode")]
    OpenCode,
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
        Agent::Codex,
        Agent::Cursor,
        Agent::OpenCode,
    ];

    /// The agent's CLI label, for display.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Pi => "pi",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::OpenCode => "opencode",
        }
    }

    /// The agent's home dir — its presence is the detection signal that gates
    /// `plug` (default selection AND explicit `--agent`: no install into an
    /// absent agent).
    pub(crate) fn agent_dir(self, home: &Path) -> PathBuf {
        let part = match self {
            Agent::ClaudeCode => ".claude",
            Agent::Pi => ".pi",
            Agent::Codex => ".codex",
            Agent::Cursor => ".cursor",
            Agent::OpenCode => ".config/opencode",
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
            Agent::Codex => home.join(".codex/skills"),
            Agent::Cursor => home.join(".cursor/skills"),
            Agent::OpenCode => home.join(".config/opencode/skills"),
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
        owned_skill_dirs_under(&self.install_path(home))
    }

    pub(crate) fn legacy_install_paths(self, home: &Path) -> Vec<PathBuf> {
        match self {
            Agent::ClaudeCode => vec![home.join(".claude/skills/square")],
            Agent::Pi => vec![home.join(".agent-square/pi-extension")],
            Agent::Cursor => vec![home.join(".cursor/skills/square")],
            // Never had an older install location.
            Agent::Codex | Agent::OpenCode => Vec::new(),
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

/// The `square-*`/`shared` dirs `plug` writes under a skill root — the set an
/// agent install owns, and the set `plug --path` writes/removes under an
/// explicit directory. Only these are removed on `unplug`, so a custom `--path`
/// folder keeps anything else it holds.
pub(crate) fn owned_skill_dirs_under(root: &Path) -> Vec<PathBuf> {
    OWNED_SKILL_DIRS
        .iter()
        .chain(RETIRED_SKILL_DIRS)
        .map(|name| root.join(name))
        .collect()
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

    /// The embedded tree is the *generated* one: exactly one `SKILL.md` per
    /// skill dir, no `shared/`, no partials, and no leftover runtime reads or
    /// unrendered slots — the whole point of the build-time renderer is that
    /// an agent never spends a round trip reading a second file.
    #[test]
    fn embedded_skills_are_single_file() {
        for skill in OWNED_SKILL_DIRS {
            let dir = SKILLS
                .get_dir(skill)
                .unwrap_or_else(|| panic!("{skill} is embedded"));
            let files: Vec<_> = dir.files().collect();
            assert_eq!(
                files.len(),
                1,
                "{skill}: exactly one embedded file, found {:?}",
                files.iter().map(|file| file.path()).collect::<Vec<_>>()
            );
            assert!(dir.dirs().next().is_none(), "{skill}: no embedded subdirs");
            let body = SKILLS
                .get_file(format!("{skill}/SKILL.md"))
                .and_then(include_dir::File::contents_utf8)
                .unwrap_or_else(|| panic!("{skill}/SKILL.md is embedded utf-8"));
            assert!(
                !body.contains("Read `../shared/") && !body.contains("Read `workflow.md`"),
                "{skill}: generated skill must not instruct runtime file reads"
            );
            assert!(
                !body.contains("<!-- include") && !body.contains("<!-- slot"),
                "{skill}: generated skill carries an unrendered directive"
            );
        }
        assert!(SKILLS.get_dir("shared").is_none());
        assert!(SKILLS.get_dir("square").is_none());
    }

    /// The *source* layout the renderer consumes: templates + partials.
    #[test]
    fn skill_sources_are_templates_plus_partials() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        for skill in OWNED_SKILL_DIRS {
            let skill_dir = root.join(skill);
            assert!(skill_dir.join("SKILL.md").is_file(), "{skill} template");
            assert!(skill_dir.join("workflow.md").is_file(), "{skill} workflow");
            // Templates are self-describing: each declares its sources with
            // include directives instead of a wiring table in build.rs.
            let template = std::fs::read_to_string(skill_dir.join("SKILL.md"))
                .unwrap_or_else(|error| panic!("{skill}/SKILL.md: {error}"));
            assert!(
                template.contains("<!-- include "),
                "{skill}: template declares no includes"
            );
        }
        for partial in [
            "daemon-session.md",
            "events.md",
            "invocation.md",
            "meta.md",
            "quiet.md",
            "reattach.md",
            "receive-loop.md",
        ] {
            assert!(root.join("shared").join(partial).is_file(), "{partial}");
        }
        // `create`/`join`/`topic` share ONE daemon flow (the parameterized
        // `shared/daemon-session.md` partial): every harness starts the daemon
        // and polls the same way, so no per-skill or per-harness adapters. A
        // Claude-Code-specific adapter is what carried the Monitor event path,
        // which truncated message bodies at 500 chars and persisted them to
        // disk. `discover` still needs Monitor — it streams until killed and
        // has no poll equivalent.
        for skill in ["square-create", "square-join", "square-topic"] {
            assert!(
                !root.join(skill).join("adapters").exists(),
                "{skill} must not reintroduce per-skill adapters"
            );
        }
        assert!(
            root.join("square-discover/adapters/claude-code.md")
                .is_file(),
            "square-discover still has a Monitor adapter (tracked follow-up)"
        );
        assert!(!root.join("shared/SKILL.md").exists());
    }

    /// The Monitor prohibition, pinned on the *generated* content: the daemon
    /// starters must never tell an agent to use a watch/push tool (it
    /// truncates message bodies and persists them to disk).
    #[test]
    fn generated_daemon_starters_never_mention_monitor() {
        for skill in ["square-create", "square-join", "square-topic"] {
            let body = SKILLS
                .get_file(format!("{skill}/SKILL.md"))
                .and_then(include_dir::File::contents_utf8)
                .unwrap_or_else(|| panic!("{skill}/SKILL.md is embedded utf-8"));
            assert!(
                !body.contains("Monitor"),
                "{skill}: generated skill must not route through a Monitor/watch tool"
            );
        }
    }

    /// A harness writes a background command's output to a file. The daemon's
    /// `--output json` stdout carries every message body, and its stderr prints
    /// the bare square id — a join credential (`Output::mesh_id_line`). The
    /// `> /dev/null 2>&1` on each such line is therefore the only thing keeping
    /// either off disk; dropping one would reintroduce the leak silently, so pin
    /// it here rather than trust review.
    ///
    /// Backgrounding leaves no trace in the rendered text to match on — the
    /// harness's background facility owns it, and the launches carry no trailing
    /// `&`. So key off what is still visible: the two commands that run long, and
    /// are therefore the only ones ever handed to that facility.
    #[test]
    fn long_running_square_commands_discard_stdout_and_stderr() {
        fn is_daemon_launch(line: &str) -> bool {
            ["create", "join", "topic"]
                .iter()
                .any(|sub| line.starts_with(&format!("agent-square {sub} ")))
        }
        fn is_bell(line: &str) -> bool {
            line.starts_with("agent-square poll ") && line.contains("--long")
        }

        let mut daemon_starters_checked = 0;
        for skill in OWNED_SKILL_DIRS {
            let path = format!("{skill}/SKILL.md");
            let body = SKILLS
                .get_file(&path)
                .and_then(include_dir::File::contents_utf8)
                .unwrap_or_else(|| panic!("{path} is embedded"));

            // `trim`, not `trim_end`: the re-armed bell sits inside a numbered
            // list, so it is indented in every skill that runs the receive loop.
            let long_running: Vec<&str> = body
                .lines()
                .map(str::trim)
                .filter(|line| is_daemon_launch(line) || is_bell(line))
                .collect();

            for line in &long_running {
                assert!(
                    line.ends_with("> /dev/null 2>&1"),
                    "{path}: a long-running command must discard stdout AND stderr, \
                     or the harness writes message bodies and the square id to a \
                     file: {line}"
                );
            }

            if ["square-create", "square-join", "square-topic"].contains(skill) {
                assert_eq!(
                    long_running.iter().filter(|line| is_daemon_launch(line)).count(),
                    1,
                    "{path}: expected exactly one daemon launch, found {long_running:?}"
                );
                assert!(
                    long_running.iter().any(|line| is_bell(line)),
                    "{path}: expected a poll bell, found {long_running:?}"
                );
                daemon_starters_checked += 1;
            }
        }
        assert_eq!(daemon_starters_checked, 3);
    }

    #[test]
    fn install_paths_are_under_home() {
        let home = Path::new("/home/x");
        assert!(
            Agent::ClaudeCode
                .install_path(home)
                .ends_with(".claude/skills")
        );
        assert!(Agent::Pi.install_path(home).ends_with(".pi/agent/skills"));
        assert!(Agent::Codex.install_path(home).ends_with(".codex/skills"));
        assert!(Agent::Cursor.install_path(home).ends_with(".cursor/skills"));
        let opencode = Agent::OpenCode.install_path(home);
        assert!(opencode.ends_with(".config/opencode/skills"));
    }

    #[test]
    fn codex_in_sync_only_when_skill_tree_matches_embedded() {
        let home = std::env::temp_dir().join(format!("agent-square-insync-{}", std::process::id()));
        let dir = Agent::Codex.install_path(&home);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!Agent::Codex.in_sync(&home));

        write_embedded_dir(&SKILLS, &dir);
        assert!(Agent::Codex.in_sync(&home));

        let file = dir.join("square-create/SKILL.md");
        let mut contents = std::fs::read_to_string(&file).unwrap();
        contents.push('\n');
        std::fs::write(&file, contents).unwrap();
        assert!(!Agent::Codex.in_sync(&home));

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

        // A stale `shared/` dir from a pre-single-file install is extra
        // content, not drift — the embedded files all still match.
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::write(dir.join("shared/extra.md"), "extra").unwrap();
        assert!(Agent::Cursor.in_sync(&home));
        std::fs::remove_file(dir.join("square-join/SKILL.md")).unwrap();
        assert!(!Agent::Cursor.in_sync(&home));

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn drift_warning_fires_only_for_a_diverged_install() {
        let home = std::env::temp_dir().join(format!("agent-square-drift-{}", std::process::id()));
        let dir = Agent::Codex.install_path(&home);
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
