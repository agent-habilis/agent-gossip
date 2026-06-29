use clap::Subcommand;

use super::output::OutputFormat;
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
    /// Forward a local TCP service to peers; prints a `🐝…` ticket on stderr.
    ///
    /// Each peer that connects with the ticket is proxied to `<host>`; one ticket
    /// serves many connections (e.g. share a local dev server).
    ListenTcp {
        /// The local `host:port` TCP service to expose (e.g. `127.0.0.1:3000`).
        host: String,
        /// Swarm id whose discovery config the pipe should use (omit ⇒ public).
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) or json (a direct connect-tcp line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Bind a local TCP port and forward each connection over the pipe.
    ///
    /// Each connection to `--addr` is forwarded to the producer's TCP target.
    ConnectTcp {
        /// The `🐝…` ticket printed by `ahsw pipe listen-tcp`.
        ticket: String,
        /// Local `host:port` to listen on (e.g. `127.0.0.1:8080`).
        #[arg(long)]
        addr: String,
        /// Output format: human (default) or json (suppresses the status line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
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
    use super::parse_rate;

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
