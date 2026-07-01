use clap::Subcommand;

use super::output::OutputFormat;
use crate::pipe::PortMapping;
use crate::protocol::SwarmId;

/// The `ahsw pipe` actions — a direct, off-gossip byte stream.
#[derive(Subcommand, Debug)]
pub(crate) enum PipeAction {
    /// Read stdin and serve it to one peer; prints the `ahsw pipe connect 🐝…`
    /// command on stdout.
    ///
    /// The ticket is a bearer capability — whoever holds it connects. The data
    /// flows over the network, not stdout, so the producer composes in a pipeline.
    Listen {
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the pipe
        /// should use, so it traverses the network like swarm members do. Omit
        /// for a public default.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Cap throughput, e.g. `100k`, `2m` (bytes/sec; `k`/`m`/`g` = 1024-based).
        /// Doubles as a way to watch the progress bar on a fast/local link.
        #[arg(long, value_parser = parse_rate)]
        throttle: Option<u64>,
        /// Output format: human (default) — a bee status + colored hint — or json,
        /// a single direct `ahsw pipe connect 🐝…` line for machines (no decoration).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
        /// Live-follow mode: stay up and serve the latest stdin to whichever
        /// single consumer is attached (discarding while none is), re-accepting
        /// on drop. The producer only quits when the source ends; the consumer
        /// reconnects by re-running `pipe connect` with the same ticket. For live
        /// sources like `tail -f`; not a file transfer.
        #[arg(long)]
        follow: bool,
    },
    /// Redeem a ticket and stream the peer's bytes to stdout.
    ///
    /// Needs nothing but the ticket — the producer's address and the swarm's
    /// discovery config travel inside it.
    Connect {
        /// The `🐝…` ticket printed by `ahsw pipe listen`.
        ticket: String,
        /// Cap throughput, e.g. `100k`, `2m` (bytes/sec; `k`/`m`/`g` = 1024-based).
        /// Doubles as a way to watch the progress bar on a fast/local link.
        #[arg(long, value_parser = parse_rate)]
        throttle: Option<u64>,
    },
    /// Forward one or more local TCP services to peers; prints a `🐝…` ticket
    /// on stdout.
    ///
    /// Each peer that connects with the ticket is proxied to `127.0.0.1:PORT`
    /// for every listed port, all multiplexed over one shared connection; one
    /// ticket serves many connections (e.g. share a local dev server + DB).
    ListenTcp {
        /// The local ports to expose, on `127.0.0.1` (e.g. `3000 5432`).
        #[arg(required = true)]
        ports: Vec<u16>,
        /// Swarm id whose discovery config the pipe should use (omit ⇒ public).
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) or json (a direct connect-tcp line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Bind one or more local TCP ports and forward each connection over the
    /// pipe.
    ///
    /// Each connection to `127.0.0.1:PORT` is forwarded to the producer's TCP
    /// target on the same port; all ports share one connection. Pass the same
    /// ports the `listen-tcp` ticket advertises.
    ConnectTcp {
        /// The `🐝…` ticket printed by `ahsw pipe listen-tcp`.
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

/// Parse a `connect-tcp` port argument: `LOCAL:REMOTE` (map across) or a bare
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

/// Parse a throttle rate like `512`, `100k`, `2m`, `1g` into bytes/sec. Suffixes
/// are 1024-based (`k` = `KiB`, `m` = `MiB`, `g` = `GiB`); a bare number is bytes.
fn parse_rate(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    let (digits, mult): (&str, u64) = match raw.chars().last() {
        // The suffix is ASCII, so trimming one byte is on a char boundary.
        Some('k' | 'K') => (&raw[..raw.len() - 1], 1 << 10),
        Some('m' | 'M') => (&raw[..raw.len() - 1], 1 << 20),
        Some('g' | 'G') => (&raw[..raw.len() - 1], 1 << 30),
        _ => (raw, 1),
    };
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("invalid rate `{raw}` (use e.g. 512, 100k, 2m)"))?;
    let bytes = value
        .checked_mul(mult)
        .ok_or_else(|| format!("rate `{raw}` is too large"))?;
    if bytes == 0 {
        return Err("rate must be greater than 0".to_owned());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{parse_port_mapping, parse_rate};

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

    #[test]
    fn parses_plain_and_suffixed_rates() {
        assert_eq!(parse_rate("512"), Ok(512));
        assert_eq!(parse_rate("100k"), Ok(100 * 1024));
        assert_eq!(parse_rate("2M"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_rate("1g"), Ok(1 << 30));
    }

    #[test]
    fn rejects_garbage_and_zero() {
        assert!(parse_rate("abc").is_err());
        assert!(parse_rate("0").is_err());
        assert!(parse_rate("12x").is_err());
    }
}
