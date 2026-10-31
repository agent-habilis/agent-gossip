//! `doctor` command args: the environment + network diagnostic. With no
//! `--gossip` it reports machine health (environment, integrations, network
//! capability, active gossips); with `--gossip <hash>` it analyzes the connection
//! methods to a specific gossip.

use clap::Parser;
use habilis_network::protocol::MeshId;

use super::output_format::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct DoctorOpts {
    /// Analyze a specific gossip: decode its declared connection
    /// methods and live-probe which actually reach it. Omit for the
    /// machine-health report.
    #[arg(long, alias = "room")]
    pub gossip: Option<MeshId>,

    /// Skip the live network probes, reporting only what's known without
    /// touching the network (static decode + local state).
    #[arg(long, default_value_t = false)]
    pub no_probe: bool,

    /// Output format: human (default) or json (the structured report).
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
        let Commands::Doctor { options } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(
            options.gossip.is_none(),
            "bare doctor checks the machine, not a gossip"
        );
        assert!(
            !options.no_probe,
            "the live probes run unless --no-probe is passed"
        );
        assert_eq!(
            options.output,
            OutputFormat::Human,
            "the default output is the human report"
        );
    }

    #[test]
    fn doctor_accepts_mesh_and_json() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "doctor",
            "--gossip",
            "AbCdEf1234",
            "--no-probe",
            "--output",
            "json",
        ]);
        let Commands::Doctor { options } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(
            options.gossip.is_some(),
            "--gossip selects a gossip to analyze"
        );
        assert!(options.no_probe, "--no-probe skips the live probes");
        assert_eq!(
            options.output,
            OutputFormat::Json,
            "--output json selects the structured report"
        );
    }
}
