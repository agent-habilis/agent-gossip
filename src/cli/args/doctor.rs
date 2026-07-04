//! `doctor` command args: the environment + network diagnostic. With no
//! `--swarm` it reports machine health (environment, integrations, network
//! capability, active swarms); with `--swarm <💬…>` it analyzes the connection
//! methods to a specific swarm.

use clap::Parser;

use crate::protocol::SwarmId;

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct DoctorOpts {
    /// Analyze a specific swarm (💬...): decode its declared connection
    /// methods and live-probe which actually reach it. Omit for the
    /// machine-health report.
    #[arg(long)]
    pub swarm: Option<SwarmId>,

    /// Skip the live network probes, reporting only what's known without
    /// touching the network (static decode + local state).
    #[arg(long, default_value_t = false)]
    pub no_probe: bool,

    /// Output format: human (default) or json (structured JSON)
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands, OutputFormat};

    #[test]
    fn doctor_defaults_to_machine_health_human() {
        let cli = Cli::parse_from(["agent-gossip", "doctor"]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.swarm.is_none());
        assert!(!opts.no_probe);
        assert_eq!(opts.output, OutputFormat::Human);
    }

    #[test]
    fn doctor_accepts_swarm_and_json() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "doctor",
            "--swarm",
            "💬AbCdEf1234",
            "--no-probe",
            "--output",
            "json",
        ]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.swarm.is_some());
        assert!(opts.no_probe);
        assert_eq!(opts.output, OutputFormat::Json);
    }
}
