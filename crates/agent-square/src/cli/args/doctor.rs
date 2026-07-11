//! `doctor` command args: the environment + network diagnostic. With no
//! `--square` it reports machine health (environment, integrations, network
//! capability, active squares); with `--square <💬…>` it analyzes the connection
//! methods to a specific square.

use clap::Parser;

use agent_habilis_mesh::protocol::MeshId;

#[derive(Parser, Debug)]
pub(crate) struct DoctorOpts {
    /// Analyze a specific square (💬...): decode its declared connection
    /// methods and live-probe which actually reach it. Omit for the
    /// machine-health report.
    #[arg(long)]
    pub square: Option<MeshId>,

    /// Skip the live network probes, reporting only what's known without
    /// touching the network (static decode + local state).
    #[arg(long, default_value_t = false)]
    pub no_probe: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn doctor_defaults_to_machine_health() {
        let cli = Cli::parse_from(["agent-square", "doctor"]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.square.is_none());
        assert!(!opts.no_probe);
    }

    #[test]
    fn doctor_accepts_mesh_and_no_probe() {
        let cli = Cli::parse_from([
            "agent-square",
            "doctor",
            "--square",
            "💬AbCdEf1234",
            "--no-probe",
        ]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.square.is_some());
        assert!(opts.no_probe);
    }
}
