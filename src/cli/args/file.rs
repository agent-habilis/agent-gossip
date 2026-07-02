use std::path::PathBuf;

use clap::Subcommand;

use super::lookup::LookupArgs;
use super::output::OutputFormat;
use super::pipe::parse_rate;
use super::shared::DirectoryTuningArgs;
use crate::protocol::SwarmId;
use crate::protocol::swarm::SwarmName;

/// The `ahsw file` actions — a direct, off-gossip file/folder transfer.
#[derive(Subcommand, Debug)]
pub(crate) enum FileAction {
    /// Send a file or folder to peers; prints the `ahsw file get 🐝…`
    /// command on stdout.
    ///
    /// Keeps serving until interrupted, re-reading the source per connection so
    /// a repeat `get` re-syncs. Only files the peer is missing or has an
    /// outdated copy of are sent (a snapshot + delta re-sync, not a live watch).
    Send {
        /// The file or directory to send.
        path: PathBuf,
        /// Swarm id whose discovery config (local / mDNS / DHT / relay) the
        /// transfer should use, so it traverses the network like swarm members
        /// do. Omit for a public default. Alternative to the
        /// `--mdns`/`--dht`/`--relay` flags — pass one or the other.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Which lookup mechanisms the transfer uses (same flags as `create`):
        /// naming any uses only those; naming none is the all-on public preset.
        #[command(flatten)]
        lookups: LookupArgs,
        /// Advertise this transfer's ticket in a directory so a peer can find
        /// it with `ahsw file discover` — no `🐝…` to copy. Bare `--advertise`
        /// ⇒ the default `global` directory; `--advertise <name>` ⇒ that named
        /// directory (share the name with the peer, like a code word). The ad
        /// carries the full bearer ticket, so anyone browsing that directory
        /// can fetch the files.
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
        #[arg(long, value_parser = parse_rate)]
        throttle: Option<u64>,
        /// Protect the transfer with a password: the ticket alone no longer
        /// redeems — receivers must present the password (so a passworded
        /// ticket is safe to --advertise). Bare `--password` prompts hidden
        /// on the terminal; `--password=<pw>` passes it inline (visible in
        /// `ps` — prefer the prompt when a human types it).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,
        /// Output format: human (default) — a cargo-style status + hint — or json,
        /// a single direct `ahsw file get 🐝…` line for machines.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Redeem a ticket and receive the tree into the current directory
    /// (a folder named after the source; a single file keeps its name),
    /// overwriting existing files.
    ///
    /// Sends the sender a manifest of what you already have, so only
    /// changed/missing files are transferred. Never deletes — a receive, not a
    /// mirror.
    Get {
        /// The `🐝…` ticket printed by `ahsw file send`.
        ticket: String,
        /// Write into this directory instead of the current one.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Cap throughput, e.g. `100k`, `2m` (bytes/sec; `k`/`m`/`g` = 1024-based).
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
        /// Output format: human (default) or json (suppresses the summary line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
    /// Browse a directory for advertised file transfers and receive one —
    /// the receiver side of `file send --advertise`, no `🐝…` to copy.
    ///
    /// Human mode runs a live picker and fetches on selection; `--output
    /// json` streams `ticket_found`/`ticket_lost` lines instead (the agent
    /// captures a ticket and runs `file get` itself).
    Discover {
        /// The directory to browse — the name the sender passed to
        /// `--advertise` (omit for the default `global` directory).
        #[arg(default_value = crate::protocol::swarm::DEFAULT_DIRECTORY)]
        name: SwarmName,
        /// Which lookup mechanisms reach the directory (same flags as
        /// `discover`): must match the sender's. Naming none is the all-on
        /// public preset.
        #[command(flatten)]
        lookups: LookupArgs,
        /// Hidden directory-tuning knobs (test suite only).
        #[command(flatten)]
        tuning: DirectoryTuningArgs,
        /// Write into this directory instead of the current one.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Cap throughput of the transfer made on pick.
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
}
