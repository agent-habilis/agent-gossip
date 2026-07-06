//! `doctor` command args: the environment + network diagnostic. With no
//! `--mesh` it reports machine health (environment, integrations, network
//! capability, active meshes); with `--mesh <💬…>` it analyzes the connection
//! methods to a specific mesh.

use clap::Parser;

use agent_habilis_mesh::protocol::MeshId;

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct DoctorOpts {
    /// Analyze a specific mesh (💬...): decode its declared connection
    /// methods and live-probe which actually reach it. Omit for the
    /// machine-health report.
    #[arg(long)]
    pub mesh: Option<MeshId>,

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
        let cli = Cli::parse_from(["agent-mesh", "doctor"]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.mesh.is_none());
        assert!(!opts.no_probe);
        assert_eq!(opts.output, OutputFormat::Human);
    }

    #[test]
    fn doctor_accepts_mesh_and_json() {
        let cli = Cli::parse_from([
            "agent-mesh",
            "doctor",
            "--mesh",
            "💬AbCdEf1234",
            "--no-probe",
            "--output",
            "json",
        ]);
        let Commands::Doctor { opts } = cli.command else {
            panic!("expected Doctor command");
        };
        assert!(opts.mesh.is_some());
        assert!(opts.no_probe);
        assert_eq!(opts.output, OutputFormat::Json);
    }
}
