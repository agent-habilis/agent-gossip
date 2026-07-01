use clap::Subcommand;

use super::output::OutputFormat;
use crate::port::PortMapping;
use crate::protocol::SwarmId;

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
        /// Swarm id whose discovery config the forward should use (omit ⇒ public).
        #[arg(long)]
        swarm: Option<SwarmId>,
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
        /// Output format: human (default) or json (suppresses the status line).
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
