use crate::TaskOutcome;
use crate::util::{output, repo_root};

/// Generate roff man pages into `target/man/` by walking the `agent-gossip` clap
/// tree (`agent_gossip::cli_command`) through `clap_mangen`, in
/// process. `generate_to` recurses into every subcommand, emitting one
/// page each (`agent-gossip.1`, `agent-gossip-create.1`, … `agent-gossip-man.1`). Output is a
/// build artifact; not checked in.
pub(crate) fn run() -> TaskOutcome {
    let out = repo_root().join("target/man");
    std::fs::create_dir_all(&out)?;
    clap_mangen::generate_to(agent_gossip::cli_command(), &out)?;
    output::status("Generated", &format!("man pages ({})", out.display()));
    Ok(())
}
