use std::process::ExitCode;

use clap::{Parser, Subcommand};
use xshell::{Shell, cmd};

/// Marketplace and plugin identifiers for the `cc-plugin-*` tasks.
/// `agent-habilis-swarm` is a directory-source marketplace pointing at
/// this repo; `swarm` is the only plugin it exposes.
const CC_MARKETPLACE: &str = "agent-habilis-swarm";
const CC_PLUGIN: &str = "swarm@agent-habilis-swarm";

/// Task result; any `Err` is printed and turns into a non-zero exit.
type TaskOutcome = Result<(), Box<dyn std::error::Error>>;

/// Project task runner. Run `cargo xtask <task>`.
#[derive(Parser)]
#[command(bin_name = "cargo xtask")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

/// Variant doc comments *are* the `--help` text — no separate usage
/// block to drift. clap kebab-cases names (`CcPluginInstall` →
/// `cc-plugin-install`), so the invocation surface stays stable.
#[derive(Subcommand)]
enum Task {
    /// Run unit tests.
    Test,
    /// Build the release binary.
    Release {
        /// `cargo-release` level (`patch`|`minor`|`major`|`x.y.z`) plus
        /// extra flags such as `--execute`. Dry run by default; with no
        /// args this just builds the release binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install the binary.
    Install,
    /// Install the Claude Code plugin.
    CcPluginInstall,
    /// Uninstall the Claude Code plugin.
    CcPluginUninstall,
    /// Run tests with coverage.
    Coverage,
    /// Run the CI gate.
    Ci,
    /// Format source files.
    Fmt,
    /// Run clippy lints.
    Lint,
    /// Remove build artifacts.
    Clean,
    /// Run property-based tests.
    Proptest,
    /// Install the pi extension.
    PiLink,
    /// Uninstall the pi extension.
    PiUnlink,
    /// Type-check the pi extension.
    PiTypecheck,
    /// Lint the pi extension.
    PiLint,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let sh = match Shell::new() {
        Ok(sh) => sh,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = match cli.task {
        Task::Test => run_test(&sh),
        Task::Release { args } => run_release(&sh, &args),
        Task::Install => run_install(&sh),
        Task::CcPluginInstall => run_cc_plugin_install(&sh),
        Task::CcPluginUninstall => run_cc_plugin_uninstall(&sh),
        Task::Coverage => run_coverage(&sh),
        Task::Ci => run_ci(&sh),
        Task::Fmt => run_fmt(&sh),
        Task::Lint => run_lint(&sh),
        Task::Clean => run_clean(&sh),
        Task::Proptest => run_proptest(&sh),
        Task::PiLink => link_pi(&sh),
        Task::PiUnlink => unlink_pi(&sh),
        Task::PiTypecheck => run_pi_typecheck(&sh),
        Task::PiLint => run_pi_lint(&sh),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_test(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo test -- --test-threads=4").quiet().run()?;
    Ok(())
}

fn run_release(sh: &Shell, args: &[String]) -> TaskOutcome {
    if let Some((level, extra)) = args.split_first() {
        ensure_installed(sh, "cargo-release", &["release", "--version"]);
        // `cargo release --execute` asks for confirmation on an interactive
        // TTY; `cargo xtask` is the non-interactive entrypoint (CI, agents),
        // so pass `--no-confirm` whenever we are actually executing. The dry
        // run (no `--execute`) never prompts, so leave it untouched.
        let mut release_args: Vec<String> = extra.to_vec();
        let executing = release_args.iter().any(|arg| arg.as_str() == "--execute");
        if executing
            && !release_args
                .iter()
                .any(|arg| arg.as_str() == "--no-confirm")
        {
            release_args.push("--no-confirm".to_owned());
        }
        eprintln!("=> cargo release {level} {}", release_args.join(" "));
        cmd!(sh, "cargo release {level} {release_args...}")
            .quiet()
            .run()?;
        return Ok(());
    }
    eprintln!("=> Building release binary...");
    cmd!(sh, "cargo build --release").quiet().run()?;
    eprintln!("=> Binary: target/release/agent-habilis-swarm");
    Ok(())
}

fn run_install(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Installing agent-habilis-swarm...");
    cmd!(sh, "cargo install --path .").quiet().run()?;
    eprintln!("=> Installed to ~/.cargo/bin/agent-habilis-swarm");
    Ok(())
}

fn run_coverage(sh: &Shell) -> TaskOutcome {
    ensure_installed(sh, "cargo-llvm-cov", &["llvm-cov", "--version"]);
    eprintln!("=> Running tests with coverage instrumentation...");
    // `--test-threads=4` bounds concurrent P2P daemons the same as
    // `run_test`/`ci` — `cargo llvm-cov` wraps `cargo test`, so the
    // integration suites would otherwise over-subscribe and flake.
    cmd!(sh, "cargo llvm-cov --no-report -- --test-threads=4")
        .quiet()
        .run()?;
    eprintln!("=> Coverage summary:");
    cmd!(sh, "cargo llvm-cov report").quiet().run()?;
    Ok(())
}

fn run_fmt(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo fmt").quiet().run()?;
    Ok(())
}

fn run_lint(sh: &Shell) -> TaskOutcome {
    cmd!(sh, "cargo clippy --all-targets -- -D warnings")
        .quiet()
        .run()?;
    Ok(())
}

fn run_clean(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Cleaning build artifacts...");
    cmd!(sh, "cargo clean").quiet().run()?;
    // llvm-cov uses a separate target dir
    let _ = sh.remove_path("target/llvm-cov-target");
    Ok(())
}

fn run_proptest(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Running property-based tests (prop_ prefix)...");
    cmd!(sh, "cargo test prop_").quiet().run()?;
    Ok(())
}

// ── pi extension linking ──────────────────────────────────────────────────

fn repo_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is xtask/, whose parent is the workspace root.
    // Falls back to CWD if the env var is somehow missing.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            std::path::Path::to_path_buf,
        )
}

fn link_pi(sh: &Shell) -> TaskOutcome {
    let src = repo_root().join("pi-extension");
    eprintln!("=> Installing pi extension...");
    cmd!(sh, "pi install {src}").quiet().run()?;
    Ok(())
}

fn unlink_pi(sh: &Shell) -> TaskOutcome {
    let src = repo_root().join("pi-extension");
    eprintln!("=> Removing pi extension...");
    cmd!(sh, "pi remove {src}").quiet().run()?;
    Ok(())
}

// ── Claude Code plugin install/remove ─────────────────────────────────────

fn command_exists(sh: &Shell, program: &str) -> bool {
    cmd!(sh, "{program} --version")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok()
}

fn run_cc_plugin_install(sh: &Shell) -> TaskOutcome {
    if !command_exists(sh, "claude") {
        return Err("`claude` CLI not found on PATH".into());
    }

    // Directory-source marketplace: refresh its snapshot. If it was
    // never registered on this machine, register it from the repo.
    eprintln!("=> Refreshing marketplace {CC_MARKETPLACE}...");
    if cmd!(sh, "claude plugin marketplace update {CC_MARKETPLACE}")
        .quiet()
        .run()
        .is_err()
    {
        let root = repo_root();
        eprintln!(
            "=> Marketplace not registered; adding {}...",
            root.display()
        );
        cmd!(sh, "claude plugin marketplace add {root}")
            .quiet()
            .run()?;
    }

    // Best-effort: a clean machine has nothing to remove.
    eprintln!("=> Uninstalling {CC_PLUGIN} (if present)...");
    let _ = cmd!(sh, "claude plugin uninstall {CC_PLUGIN} -y")
        .quiet()
        .run();

    eprintln!("=> Installing {CC_PLUGIN} from this repo...");
    cmd!(sh, "claude plugin install {CC_PLUGIN}")
        .quiet()
        .run()?;
    eprintln!("=> Reinstalled. A running Claude session still needs");
    eprintln!("   /reload-plugins; new `claude` invocations pick it up.");
    Ok(())
}

fn run_cc_plugin_uninstall(sh: &Shell) -> TaskOutcome {
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

fn run_ci(sh: &Shell) -> TaskOutcome {
    eprintln!("=> Checking formatting...");
    cmd!(sh, "cargo fmt --check").quiet().run()?;

    eprintln!("=> Running clippy...");
    cmd!(sh, "cargo clippy --all-targets -- -D warnings")
        .quiet()
        .run()?;

    eprintln!("=> Running tests...");
    cmd!(sh, "cargo test -- --test-threads=4").quiet().run()?;

    // The reliability invariants (ungraceful SIGKILL death, sleep/wake
    // heal recovery, anti-entropy backfill) run here every time — they
    // live in `tests/gossip_network.rs` with shortened eviction timers.

    run_pi_typecheck(sh)?;
    run_pi_lint(sh)
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

fn run_pi_typecheck(sh: &Shell) -> TaskOutcome {
    ensure_pi_deps(sh)?;
    eprintln!("=> Type-checking pi-extension...");
    cmd!(sh, "npm --prefix pi-extension run typecheck")
        .quiet()
        .run()?;
    Ok(())
}

fn run_pi_lint(sh: &Shell) -> TaskOutcome {
    ensure_pi_deps(sh)?;
    eprintln!("=> Linting pi-extension...");
    cmd!(sh, "npm --prefix pi-extension run lint")
        .quiet()
        .run()?;
    Ok(())
}

/// Install `krate` via `cargo install --locked` if the probe command
/// (`check`) fails. Best-effort: a probe or install hiccup must not
/// abort the calling task — its own command surfaces a clear error if
/// the tool is genuinely missing.
fn ensure_installed(sh: &Shell, krate: &str, check: &[&str]) {
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
