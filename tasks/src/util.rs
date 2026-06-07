use std::path::{Path, PathBuf};

use anstyle::{AnsiColor, Style};
use xshell::{Shell, cmd};

use crate::Target;

// ── cargo-style status output (mirrors `../browse/src/util/style.rs`) ────────

/// Print a cargo-style status line: a right-aligned (12-col), bold-green `verb`
/// then `msg`. Written through `anstream`, which strips color when stderr isn't
/// a terminal — so piped/agent output stays clean.
pub(crate) fn status(verb: &str, msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Green.into())).bold();
    anstream::eprintln!("{style}{verb:>12}{style:#} {msg}");
}

/// Print a cargo-style `warning: {msg}` line (bold-yellow `warning`).
pub(crate) fn warn(msg: &str) {
    let style = Style::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
    anstream::eprintln!("{style}warning{style:#}: {msg}");
}

/// Every target, in display order — the default when no `--target` is given.
const ALL: [Target; 2] = [Target::CcPlugin, Target::Pi];

/// Which targets to act on: the explicit `--target` flags, or — when none are
/// given — all of them. Flag-driven only; no prompts. Shared by `setup::run`
/// and `teardown::run` so the two stay symmetric.
pub(crate) fn resolve(explicit: &[Target]) -> Vec<Target> {
    if explicit.is_empty() {
        ALL.to_vec()
    } else {
        explicit.to_vec()
    }
}

/// Workspace root: the parent of this crate's `tasks/` manifest dir.
/// Falls back to CWD if the env var is somehow missing.
pub(crate) fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is tasks/, whose parent is the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            Path::to_path_buf,
        )
}

/// Install `krate` via `cargo install --locked` if the probe command
/// (`check`) fails. Best-effort: a probe or install hiccup must not
/// abort the calling task — its own command surfaces a clear error if
/// the tool is genuinely missing.
pub(crate) fn ensure_installed(sh: &Shell, krate: &str, check: &[&str]) {
    let ok = cmd!(sh, "cargo {check...}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();

    if !ok {
        eprintln!("=> Installing {krate}...");
        let _ = cmd!(sh, "cargo install --locked {krate}").quiet().run();
    }
}

/// Prune `target/` artifacts not touched in the last week. Old build
/// generations from past sessions pile up (feature-flag/profile permutations
/// and `[patch]` source swaps have pushed this tree to tens of GB), while the
/// build that just ran is touched *now* and is always kept. Best-effort:
/// installs `cargo-sweep` on demand and never aborts the calling task.
pub(crate) fn sweep_stale_artifacts(sh: &Shell) {
    ensure_installed(sh, "cargo-sweep", &["sweep", "--version"]);
    eprintln!("=> Pruning build artifacts older than 7 days (cargo sweep)...");
    let _ = cmd!(sh, "cargo sweep --time 7").quiet().run();
}

// ── Shared cc-plugin (skills-dir) helpers, used by both `setup` and ──────────
// `teardown` so neither owns the other's code.

/// The plugin loads as a **skills-directory plugin**: a folder under
/// `~/.claude/skills/<name>/` that holds a `.claude-plugin/plugin.json` is
/// discovered in place as `<name>@skills-dir` — no marketplace, no install
/// step. `setup` symlinks the repo's `claude-code-plugin` there.
pub(crate) const SKILL_LINK_NAME: &str = "swarm";

/// Transitional: the directory marketplace this plugin used to install through.
/// Best-effort dropped so a machine migrating off the marketplace flow doesn't
/// load `swarm:` twice (cached marketplace copy + skills-dir).
const LEGACY_MARKETPLACE: &str = "agent-habilis-swarm";

/// `~/.claude/skills/swarm` — where the skills-dir plugin lives.
pub(crate) fn skills_link() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".claude/skills")
        .join(SKILL_LINK_NAME))
}

/// Best-effort remove a prior skills-dir entry — our symlink, a stale broken
/// symlink, or a real directory. `symlink_metadata` does not follow the link,
/// so a dangling symlink is still seen and removed. Absent is success.
pub(crate) fn remove_existing(link: &Path) {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(link);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(link);
        }
        Err(_) => {}
    }
}

/// Drop the transitional directory marketplace if `claude` is on PATH. Errors
/// are ignored: the desired end state (deregistered) is idempotent, and the
/// skills-dir symlink does not need the CLI at all.
pub(crate) fn drop_legacy_marketplace(sh: &Shell) {
    if !command_exists(sh, "claude") {
        return;
    }
    let _ = cmd!(sh, "claude plugin marketplace remove {LEGACY_MARKETPLACE}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run();
}

fn command_exists(sh: &Shell, program: &str) -> bool {
    cmd!(sh, "{program} --version")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok()
}
