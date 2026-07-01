use clap::Subcommand;

use super::output::OutputFormat;
use crate::protocol::SwarmId;

/// The `ahsw port` actions — TCP forwarding over a direct, off-gossip link.
#[derive(Subcommand, Debug)]
pub(crate) enum PortAction {
    /// Forward a local TCP service to peers; prints a `🐝…` ticket on stdout.
    ///
    /// Each peer that connects with the ticket is proxied to `127.0.0.1:PORT`;
    /// one ticket serves many connections (e.g. share a local dev server).
    Listen {
        /// The local port to expose, on `127.0.0.1` (e.g. `3000`).
        port: u16,
        /// Swarm id whose discovery config the forward should use (omit ⇒ public).
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) or json (a direct connect line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Bind a local TCP port and forward each connection to the producer.
    ///
    /// Each connection to `127.0.0.1:PORT` is forwarded to the producer's TCP
    /// target.
    Connect {
        /// The `🐝…` ticket printed by `ahsw port listen`.
        ticket: String,
        /// Local port to listen on, on `127.0.0.1` (e.g. `8080`).
        port: u16,
        /// Output format: human (default) or json (suppresses the status line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
}
