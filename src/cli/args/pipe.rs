use clap::Subcommand;

use super::lookup::LookupArgs;
use super::output::OutputFormat;
use super::shared::DirectoryTuningArgs;
use crate::pipe::BenchBudget;
use crate::protocol::SwarmId;
use crate::protocol::swarm::SwarmName;

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
        /// for a public default. Alternative to the `--mdns`/`--dht`/`--relay`
        /// flags — pass one or the other.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Which lookup mechanisms the pipe uses (same flags as `create`):
        /// naming any uses only those; naming none is the all-on public preset.
        #[command(flatten)]
        lookups: LookupArgs,
        /// Advertise this pipe's ticket in a directory so a peer can find it
        /// with `ahsw pipe discover` — no `🐝…` to copy. Bare `--advertise` ⇒
        /// the default `global` directory; `--advertise <name>` ⇒ that named
        /// directory (share the name with the peer, like a code word). The ad
        /// carries the full bearer ticket, so anyone browsing that directory
        /// can connect. Needs a re-servable source: a seekable file or --follow.
        #[arg(long, num_args(0..=1))]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct directory states (see DirectorySelection)"
        )]
        advertise: Option<Option<SwarmName>>,
        /// Hidden directory-tuning knobs (test suite only).
        #[command(flatten)]
        tuning: DirectoryTuningArgs,
        /// Cap throughput, e.g. `100k`, `2m` (bytes/sec; `k`/`m`/`g` = 1024-based).
        /// Doubles as a way to watch the progress bar on a fast/local link.
        #[arg(long, value_parser = parse_rate)]
        throttle: Option<u64>,
        /// Output format: human (default) — a bee status + colored hint — or json,
        /// a single direct `ahsw pipe connect 🐝…` line for machines (no decoration).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
        /// Protect the pipe with a password: the ticket alone no longer
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
        /// Password for a password-protected ticket — required exactly when
        /// the ticket carries the password flag (as a prompt on a terminal,
        /// or inline via `--password=<pw>`).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
    },
    /// Browse a directory for advertised pipes and connect to one — the
    /// receiver side of `pipe listen --advertise`, no `🐝…` to copy.
    ///
    /// Human mode runs a live picker and connects on selection; `--output
    /// json` streams `ticket_found`/`ticket_lost` lines instead (the agent
    /// captures a ticket and runs `pipe connect` itself).
    Discover {
        /// The directory to browse — the name the producer passed to
        /// `--advertise` (omit for the default `global` directory).
        #[arg(default_value = crate::protocol::swarm::DEFAULT_DIRECTORY)]
        name: SwarmName,
        /// Which lookup mechanisms reach the directory (same flags as
        /// `discover`): must match the advertiser's, so an mDNS-only pipe is
        /// found only over `--mdns`. Naming none is the all-on public preset.
        #[command(flatten)]
        lookups: LookupArgs,
        /// Hidden directory-tuning knobs (test suite only).
        #[command(flatten)]
        tuning: DirectoryTuningArgs,
        /// Cap throughput of the connection made on pick.
        #[arg(long, value_parser = parse_rate)]
        throttle: Option<u64>,
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
        /// Password: on the producer, protect the minted ticket (consumers
        /// must present it); on the consumer, present it for a protected
        /// ticket. Bare `--password` prompts hidden; `--password=<pw>` inline.
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
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
