//! Offline roff man-page generator. Walks the `ah-s` clap command tree
//! (via the lib's iroh-free [`agent_habilis_swarm::cli_command`]) and
//! writes one roff page per (sub)command into `--out`: `ah-s.1`,
//! `ah-s-create.1`, `ah-s-join.1`, and so on through `ah-s-man.1`.
//!
//! Built only under the `mangen` feature (a `required-features` bin), so
//! `clap_mangen` never lands in the shipped `ah-s` binary. Driven by
//! `cargo task man`; the release workflow runs it to bundle the pages
//! Homebrew installs.

use std::path::PathBuf;

use clap::Parser;

/// Generate roff man pages for `ah-s` into a directory.
#[derive(Parser)]
struct Args {
    /// Directory to write the `.1` files into (created if missing).
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out)?;
    // `generate_to` recurses into every subcommand, emitting one page each.
    clap_mangen::generate_to(agent_habilis_swarm::cli_command(), &args.out)?;
    eprintln!("man pages written to {}", args.out.display());
    Ok(())
}
