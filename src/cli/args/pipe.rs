use clap::Subcommand;

use super::output::OutputFormat;
use crate::pipe::BenchBudget;
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
    /// Forward a local TCP service to peers; prints a `🐝…` ticket on stdout.
    ///
    /// Each peer that connects with the ticket is proxied to `127.0.0.1:PORT`;
    /// one ticket serves many connections (e.g. share a local dev server).
    ListenTcp {
        /// The local port to expose, on `127.0.0.1` (e.g. `3000`).
        port: u16,
        /// Swarm id whose discovery config the pipe should use (omit ⇒ public).
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) or json (a direct connect-tcp line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Bind a local TCP port and forward each connection over the pipe.
    ///
    /// Each connection to `127.0.0.1:PORT` is forwarded to the producer's TCP
    /// target.
    ConnectTcp {
        /// The `🐝…` ticket printed by `ahsw pipe listen-tcp`.
        ticket: String,
        /// Local port to listen on, on `127.0.0.1` (e.g. `8080`).
        port: u16,
        /// Output format: human (default) or json (suppresses the status line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Serve one throughput + latency benchmark run; prints the consumer's
    /// `ahsw pipe connect-bench 🐝…` command on stdout.
    ///
    /// Runs a single benchmark against the first peer that connects, then
    /// exits — re-run for another.
    ListenBench {
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the pipe
        /// should use, so it traverses the network like swarm members do. Omit
        /// for a public default.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Output format: human (default) — a bee status + colored hint — or json,
        /// a single direct `ahsw pipe connect-bench 🐝…` line for machines.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Redeem a bench ticket and run a throughput + round-trip-latency
    /// benchmark against the producer, printing a report when done.
    ///
    /// Data flows consumer → producer (the opposite direction from
    /// `connect`): the consumer drives and times the run, the producer
    /// reports back what it actually received.
    ConnectBench {
        /// The `🐝…` ticket printed by `ahsw pipe listen-bench`.
        ticket: String,
        /// How much of the throughput phase to run: a duration (`10s`, `2m`,
        /// `1h`) or a byte count (`500b`, `100kb`, `50mb`, `2gb`). Defaults to
        /// `10s`.
        #[arg(long, value_parser = crate::pipe::parse_budget)]
        budget: Option<BenchBudget>,
        /// Number of sequential ping/pong round-trips in the latency phase.
        #[arg(long, default_value_t = 20)]
        pings: u32,
        /// Output format: human (default) — a report box — or json, a single
        /// machine-readable object.
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
