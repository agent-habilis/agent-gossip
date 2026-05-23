use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::ensure_installed;

pub(crate) fn run(sh: &Shell, args: &[String]) -> TaskOutcome {
    if let Some((level, extra)) = args.split_first() {
        ensure_installed(sh, "cargo-release", &["release", "--version"]);
        // `cargo release --execute` asks for confirmation on an interactive
        // TTY; `cargo task` is the non-interactive entrypoint (CI, agents),
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
    eprintln!("=> Binary: target/release/ahs");
    Ok(())
}
