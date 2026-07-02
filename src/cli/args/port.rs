use clap::Subcommand;

use super::lookup::LookupArgs;
use super::output::OutputFormat;
use super::shared::DirectoryTuningArgs;
use crate::port::PortMapping;
use crate::protocol::SwarmId;
use crate::protocol::swarm::SwarmName;

/// The `ahsw port` actions — TCP forwarding over a direct, off-gossip link.
#[derive(Subcommand, Debug)]
pub(crate) enum PortAction {
    /// Forward one or more local TCP services to peers; prints a `🐝…` ticket
    /// on stdout.
    ///
    /// Each peer that connects with the ticket is proxied to `127.0.0.1:PORT`
    /// for every listed port, all multiplexed over one shared connection; one
    /// ticket serves many connections (e.g. share a local dev server + DB).
    Listen {
        /// The local ports to expose, on `127.0.0.1` (e.g. `3000 5432`).
        #[arg(required = true)]
        ports: Vec<u16>,
        /// Swarm id whose discovery config the forward should use (omit ⇒
        /// public). Alternative to the `--mdns`/`--dht`/`--relay` flags —
        /// pass one or the other.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Which lookup mechanisms the forward uses (same flags as `create`):
        /// naming any uses only those; naming none is the all-on public preset.
        #[command(flatten)]
        lookups: LookupArgs,
        /// Advertise this forward's ticket in a directory so a peer can find
        /// it with `ahsw port discover` — no `🐝…` to copy. Bare `--advertise`
        /// ⇒ the default `global` directory; `--advertise <name>` ⇒ that named
        /// directory (share the name with the peer, like a code word). The ad
        /// carries the full bearer ticket, so anyone browsing that directory
        /// can forward to these ports.
        #[arg(long, num_args(0..=1))]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct directory states (see DirectorySelection)"
        )]
        advertise: Option<Option<SwarmName>>,
        /// Hidden directory-tuning knobs (test suite only).
        #[command(flatten)]
        tuning: DirectoryTuningArgs,
        /// Protect the forward with a password: the ticket alone no longer
        /// redeems — consumers must present the password (so a passworded
        /// ticket is safe to --advertise). Bare `--password` prompts hidden
        /// on the terminal; `--password=<pw>` passes it inline (visible in
        /// `ps` — prefer the prompt when a human types it).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
        /// Output format: human (default) or json (a direct connect line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Bind one or more local TCP ports and forward each connection to the
    /// producer.
    ///
    /// Each connection to `127.0.0.1:LOCAL` is forwarded to the producer's
    /// REMOTE target port; all ports share one connection.
    Connect {
        /// The `🐝…` ticket printed by `ahsw port listen`.
        ticket: String,
        /// Ports to forward, as `LOCAL:REMOTE` (e.g. `8080:3000` binds local
        /// 8080 → the producer's 3000). A bare `PORT` maps a port to itself
        /// (`3000` == `3000:3000`). Each REMOTE must be one the ticket exposes.
        #[arg(required = true, value_parser = parse_port_mapping)]
        ports: Vec<PortMapping>,
        /// Password for a password-protected ticket — required exactly when
        /// the ticket carries the password flag (as a prompt on a terminal,
        /// or inline via `--password=<pw>`).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
        /// Output format: human (default) or json (suppresses the status line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Browse a directory for advertised port forwards and connect to one —
    /// the receiver side of `port listen --advertise`, no `🐝…` to copy.
    ///
    /// Human mode runs a live picker and forwards on selection; `--output
    /// json` streams `ticket_found`/`ticket_lost` lines instead (the agent
    /// captures a ticket and runs `port connect` itself).
    Discover {
        /// The directory to browse — the name the producer passed to
        /// `--advertise` (omit for the default `global` directory).
        #[arg(default_value = crate::protocol::swarm::DEFAULT_DIRECTORY)]
        name: SwarmName,
        /// Ports to forward on pick, as `LOCAL:REMOTE` or a bare `PORT`
        /// (maps to itself). Omit to forward **every** advertised port to
        /// the same local port.
        #[arg(value_parser = parse_port_mapping)]
        ports: Vec<PortMapping>,
        /// Which lookup mechanisms reach the directory (same flags as
        /// `discover`): must match the advertiser's. Naming none is the
        /// all-on public preset.
        #[command(flatten)]
        lookups: LookupArgs,
        /// Hidden directory-tuning knobs (test suite only).
        #[command(flatten)]
        tuning: DirectoryTuningArgs,
        /// Password for a password-protected pick (🔒 in the picker) —
        /// prompts on pick when omitted.
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
        /// Output format: human (default) — the live picker — or json, one
        /// `ticket_found`/`ticket_lost` line per directory change.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
}

/// Parse a `port connect` port argument: `LOCAL:REMOTE` (map across) or a bare
/// `PORT` (map to itself). Both ports must be non-zero `u16`s.
fn parse_port_mapping(raw: &str) -> Result<PortMapping, String> {
    let parse_port = |value: &str| -> Result<u16, String> {
        match value.trim().parse::<u16>() {
            Ok(0) | Err(_) => Err(format!(
                "invalid port `{value}` (use a number 1-65535, e.g. 8080 or 8080:3000)"
            )),
            Ok(port) => Ok(port),
        }
    };
    if let Some((local, remote)) = raw.split_once(':') {
        Ok(PortMapping {
            local: parse_port(local)?,
            remote: parse_port(remote)?,
        })
    } else {
        let port = parse_port(raw)?;
        Ok(PortMapping {
            local: port,
            remote: port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_port_mapping;

    #[test]
    fn parses_bare_port_as_self_mapping() {
        let mapping = parse_port_mapping("3000").expect("bare port");
        assert_eq!((mapping.local, mapping.remote), (3000, 3000));
    }

    #[test]
    fn parses_local_remote_mapping() {
        let mapping = parse_port_mapping("8080:3000").expect("mapping");
        assert_eq!((mapping.local, mapping.remote), (8080, 3000));
    }

    #[test]
    fn rejects_zero_garbage_and_malformed_mappings() {
        assert!(parse_port_mapping("0").is_err());
        assert!(parse_port_mapping("8080:0").is_err());
        assert!(parse_port_mapping("abc").is_err());
        assert!(parse_port_mapping("8080:").is_err());
        assert!(parse_port_mapping(":3000").is_err());
        assert!(parse_port_mapping("99999").is_err());
    }
}
