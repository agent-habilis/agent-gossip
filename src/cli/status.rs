//! `ahsw status`: report this machine's swarm setup in cargo-style groups — the
//! binary's version, then each agent's integration state (set up / not set up /
//! absent). The read-only counterpart to `plug`/`unplug`; mirrors
//! `../browse`'s `ah-b status`.

use anyhow::Result;

use crate::util::output;

use super::agent::{self, AgentState};

pub(super) fn run() -> Result<()> {
    output::status(
        "ahsw",
        &format!("version: {}", crate::util::version::VERSION),
    );

    let mut warnings = Vec::new();
    for (agent, path, state) in agent::states(&agent::home_dir()?) {
        let msg = format!(
            "{}: {} ({})",
            agent.label(),
            state.label(),
            output::home_path(&path)
        );
        match state {
            AgentState::UpToDate => output::status("skill", &msg),
            AgentState::OutOfDate => {
                output::status_warn("skill", &msg);
                warnings.push(format!(
                    "{} skill is out of date — run `ahsw plug --agent {}` to update",
                    agent.label(),
                    agent.label()
                ));
            }
            AgentState::NotSetUp | AgentState::Absent => output::status_warn("skill", &msg),
        }
    }
    for warning in &warnings {
        output::warn(warning);
    }
    Ok(())
}
