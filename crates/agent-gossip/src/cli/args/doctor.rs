//! `doctor` command args: the environment + network diagnostic. With no
//! `--room` it reports machine health (environment, integrations, network
//! capability, active rooms); with `--room <💬…>` it analyzes the connection
//! methods to a specific room.

use clap::{Parser, ValueEnum};

use agent_habilis_mesh::protocol::MeshId;

/// `doctor` is the one operator-facing report, so unlike every other command it
/// renders for a human by default and keeps the machine form behind `--output
/// json`. The agent commands stay JSON-only and keep the hidden
/// [`super::legacy::LegacyOutput`] no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

#[derive(Parser, Debug)]
pub(crate) struct DoctorOpts {
    /// Analyze a specific room (💬...): decode its declared connection
    /// methods and live-probe which actually reach it. Omit for the
    /// machine-health report.
    #[arg(long)]
    pub room: Option<MeshId>,

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
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.room.is_none());
        assert!(!opts.no_probe);
        assert_eq!(opts.output, OutputFormat::Human);
    }

    #[test]
    fn doctor_accepts_mesh_and_json() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "doctor",
            "--room",
            "💬AbCdEf1234",
            "--no-probe",
            "--output",
            "json",
        ]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.room.is_some());
        assert!(opts.no_probe);
        assert_eq!(opts.output, OutputFormat::Json);
    }
}
