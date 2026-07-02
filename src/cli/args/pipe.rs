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
        /// Live-follow mode: stay up and fan stdin out to **every** attached
        /// consumer at once, buffering the backlog while none is attached and
        /// delivering it on connect. The producer only quits when the source
        /// ends; a consumer (re)connects by running `pipe connect` with the same
        /// ticket. For live sources like `tail -f`; not a file transfer.
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
    /// Benchmark a direct pipe link's throughput + round-trip latency.
    ///
    /// With no ticket, act as the producer: bind, print the `ahsw pipe bench 🐝…`
    /// command on stdout, serve one run against the first peer that connects,
    /// then exit — re-run for another, or pass `--serve` to stay up and serve
    /// repeated runs. With a ticket, act as the consumer: redeem it and drive the
    /// run, printing a report when done. Data flows consumer → producer (the
    /// opposite direction from `connect`): the consumer drives and times the run,
    /// the producer reports what it actually received.
    Bench {
        /// The `🐝…` ticket printed by a producer-side `ahsw pipe bench`. Omit
        /// this argument to be the producer instead.
        ticket: Option<String>,
        /// [producer] Stay up and serve one benchmark per connecting peer,
        /// sequentially, until killed (instead of exiting after the first run).
        /// The ticket stays valid for the producer's whole lifetime, so reconnect
        /// any time by re-running `ahsw pipe bench 🐝…`.
        #[arg(long, conflicts_with = "ticket")]
        serve: bool,
        /// [producer] Swarm id whose discovery config (local / mDNS / DHT /
        /// relay) the pipe should use, so it traverses the network like swarm
        /// members do. Omit for a public default.
        #[arg(long, conflicts_with = "ticket")]
        swarm: Option<SwarmId>,
        /// [consumer] How much of the throughput phase to run: a duration
        /// (`10s`, `2m`, `1h`) or a byte count (`500b`, `100kb`, `50mb`, `2gb`).
        /// Defaults to `10s`.
        #[arg(long, requires = "ticket", value_parser = crate::pipe::parse_budget)]
        budget: Option<BenchBudget>,
        /// [consumer] Number of sequential ping/pong round-trips in the latency
        /// phase. Defaults to `20`.
        #[arg(long, requires = "ticket")]
        pings: Option<u32>,
        /// Output format: human (default) — a bee status + colored hint on the
        /// producer, a report box on the consumer — or json, a single
        /// machine-readable line/object.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
}

/// Parse a throttle rate like `512`, `100k`, `2m`, `1g` into bytes/sec. Suffixes
/// are 1024-based (`k` = `KiB`, `m` = `MiB`, `g` = `GiB`); a bare number is bytes.
/// Shared with `file` (the other throttled transfer command).
pub(super) fn parse_rate(raw: &str) -> Result<u64, String> {
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
