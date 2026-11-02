use crate::TaskOutcome;
use crate::util::{Invocation, output, repo_root};

pub(crate) fn run() -> TaskOutcome {
    output::status("Cleaning", "build artifacts");
    Invocation::cargo(["clean"]).quiet().run()?;
    // llvm-cov uses a separate target dir
    let _ = std::fs::remove_dir_all(repo_root().join("target/llvm-cov-target"));
    Ok(())
}
