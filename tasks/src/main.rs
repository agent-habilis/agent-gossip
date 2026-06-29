use std::process::ExitCode;

use clap::{Parser, Subcommand};
use xshell::Shell;

mod bench;
mod build;
mod ci;
mod clean;
mod coverage;
mod fmt;
mod install;
mod lint;
mod logs;
mod man;
mod pi;
mod proptest;
mod release;
mod run;
mod test;
mod util;

/// Task result; any `Err` is printed and turns into a non-zero exit.
pub(crate) type TaskOutcome = Result<(), Box<dyn std::error::Error>>;

/// Project task runner. Run `cargo task <task>`.
#[derive(Parser)]
#[command(bin_name = "cargo task")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

/// Variant doc comments *are* the `--help` text — no separate usage
/// block to drift. clap kebab-cases names (`PiTypecheck` →
/// `pi-typecheck`), so the invocation surface stays stable.
#[derive(Subcommand)]
enum Task {
    /// Run unit tests.
    Test,
    /// Build the `ahs` binary. Cross-compile with `--target <triple>` or the
    /// `--arch <arch>` shorthand (static-musl Linux, for the Pi fleet) through a
    /// project-pinned zig toolchain — self-contained, never the global zig.
    Build {
        /// Full target triple, e.g. `aarch64-unknown-linux-musl`.
        #[arg(long)]
        target: Option<String>,
        /// Architecture shorthand ⇒ `<arch>-unknown-linux-musl` (e.g.
        /// `aarch64`, `x86_64`). Mutually exclusive with `--target`.
        #[arg(long)]
        arch: Option<String>,
        /// Optimized release build (default: debug).
        #[arg(long)]
        release: bool,
    },
    /// Run benchmarks. No args = everything; `transfer` = the loopback
    /// transfer soak only; any other arg = a divan filter for the
    /// microbenchmarks (e.g. `bench derive_secret`).
    Bench {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build the release binary.
    Release {
        /// `cargo-release` level (`patch`|`minor`|`major`|`x.y.z`) plus
        /// extra flags such as `--execute`. Dry run by default; with no
        /// args this just builds the release binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the binary (`cargo run`). Extra args go to `ahsw`
    /// (e.g. `cargo task run create`).
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install the binary.
    Install,
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
    /// Print the logs directory (creating it if missing).
    Logs,
    /// Generate roff man pages into `target/man/` (needs `clap_mangen`).
    Man,
    /// Run property-based tests.
    Proptest,
    /// Type-check the pi extension.
    PiTypecheck,
    /// Lint the pi extension.
    PiLint,
    /// Run the pi extension's bun test suite.
    PiTest,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let sh = match Shell::new() {
        Ok(sh) => sh,
        Err(error) => {
            util::output::error(&error.to_string());
            return ExitCode::FAILURE;
        }
    };

    let outcome = match cli.task {
        Task::Test => test::run(&sh),
        Task::Build {
            target,
            arch,
            release,
        } => build::run(&sh, target.as_deref(), arch.as_deref(), release),
        Task::Bench { args } => bench::run(&sh, &args),
        Task::Release { args } => release::run(&sh, &args),
        Task::Run { args } => run::run(&sh, &args),
        Task::Install => install::run(&sh),
        Task::Coverage => coverage::run(&sh),
        Task::Ci => ci::run(&sh),
        Task::Fmt => fmt::run(&sh),
        Task::Lint => lint::run(&sh),
        Task::Clean => clean::run(&sh),
        Task::Logs => logs::run(),
        Task::Man => man::run(),
        Task::Proptest => proptest::run(&sh),
        Task::PiTypecheck => pi::typecheck(&sh),
        Task::PiLint => pi::lint(&sh),
        Task::PiTest => pi::test(&sh),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            util::output::error(&error.to_string());
            ExitCode::FAILURE
        }
    }
}
