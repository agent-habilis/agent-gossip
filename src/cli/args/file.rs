use std::path::PathBuf;

use clap::Subcommand;

use super::output::OutputFormat;
use super::pipe::parse_rate;
use crate::protocol::SwarmId;

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
        /// do. Omit for a public default.
        #[arg(long)]
        swarm: Option<SwarmId>,
        /// Cap throughput, e.g. `100k`, `2m` (bytes/sec; `k`/`m`/`g` = 1024-based).
        #[arg(long, value_parser = parse_rate)]
        throttle: Option<u64>,
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
        /// Output format: human (default) or json (suppresses the summary line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },
}
