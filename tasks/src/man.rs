use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::repo_root;

/// Generate roff man pages into `target/man/` by running the feature-gated
/// `gen-man` bin (which pulls `clap_mangen`, kept out of the shipped
/// binary). Output is a build artifact; not checked in.
pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    let out = repo_root().join("target/man");
    cmd!(
        sh,
        "cargo run --quiet --features mangen --bin gen-man -- --out {out}"
    )
    .run()?;
    eprintln!("=> man pages written to {}", out.display());
    Ok(())
}
