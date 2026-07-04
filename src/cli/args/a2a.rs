//! `a2a` command args: bridge a local A2A (agent-to-agent) HTTP server to a peer
//! over the swarm. `a2a expose` runs next to the server and prints a `📡…`
//! ticket; `a2a connect` redeems it and binds a local endpoint a client points
//! at. Strictly 1:1 — one consumer per exposer at a time.

use clap::{Parser, Subcommand};

use super::lookup::PublicLookupArgs;
use super::output::OutputFormat;
use crate::protocol::swarm::SwarmName;

#[derive(Parser, Debug)]
pub(crate) struct A2aOpts {
    #[command(subcommand)]
    pub action: A2aAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum A2aAction {
    /// Bridge a local A2A server to a peer; prints a `📡…` ticket on stdout.
    ///
    /// Binds an endpoint next to the A2A server named by `--to` and forwards
    /// each connecting peer's requests to it, rewriting the Agent Card's URLs so
    /// discovery resolves through the tunnel. Strictly 1:1 — one consumer is
    /// served at a time; a second is refused until the first disconnects.
    Expose {
        /// The local A2A origin to bridge, as a plain http URL (e.g.
        /// `http://127.0.0.1:9999`). https and path-prefixed origins are out of
        /// scope — the bridge forwards raw TCP to a plain-http localhost origin.
        #[arg(long)]
        to: String,

        /// Which lookup mechanisms the bridge uses (same flags as `create`):
        /// naming any uses only those; naming none (or `--public`) is the all-on
        /// public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,

        /// Advertise this bridge's ticket in a directory so a peer can find it
        /// with `agent-gossip a2a discover` — no `📡…` to copy. Bare `--advertise` ⇒ the
        /// default `global` directory; `--advertise <name>` ⇒ that named
        /// directory (share the name with the peer). The ad carries the full
        /// bearer ticket, so pair it with `--password`.
        #[arg(long, num_args(0..=1))]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct directory states (see DirectorySelection)"
        )]
        advertise: Option<Option<SwarmName>>,

        /// Protect the bridge with a password: the ticket alone no longer
        /// redeems — consumers must present the password (so a passworded ticket
        /// is safe to share). Bare `--password` prompts hidden on the terminal;
        /// `--password=<pw>` passes it inline (visible in `ps` — prefer the
        /// prompt when a human types it).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,

        /// Force loopback-only lookups so two agents on one machine bridge
        /// hermetically off the ticket's direct address (no mDNS/DHT/relay).
        /// Hidden — a testing knob.
        #[arg(long, hide = true, default_value_t = false)]
        loopback: bool,

        /// Output format: human (default) or json (a bare connect line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },

    /// Redeem a `📡…` ticket and bind a local A2A endpoint for a client.
    ///
    /// Binds `127.0.0.1:PORT` (an ephemeral port unless `--port` is given) that
    /// an unmodified A2A client/SDK points at; forwards every request to the
    /// exposer over the swarm and rewrites the Agent Card so the client
    /// discovers the local bridge, not the unreachable origin.
    Connect {
        /// The `📡…` ticket printed by `agent-gossip a2a expose`.
        ticket: String,

        /// Local port to bind the bridge on (default: an ephemeral port — the
        /// bound URL is printed).
        #[arg(long)]
        port: Option<u16>,

        /// Password for a password-protected ticket — required exactly when the
        /// ticket carries the password flag (prompts on a terminal, or inline
        /// via `--password=<pw>`).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,

        /// Output format: human (default) or json (prints the bound URL only).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },

    /// Browse a directory for advertised a2a bridges and connect to one — the
    /// receiver side of `a2a expose --advertise`, no `📡…` to copy.
    ///
    /// Human mode runs a live picker and binds a local endpoint on selection;
    /// `--output json` streams `ticket_found`/`ticket_lost` lines instead (the
    /// agent captures a ticket and runs `a2a connect` itself).
    Discover {
        /// The directory to browse — the name the exposer passed to
        /// `--advertise` (omit for the default `global` directory).
        #[arg(long)]
        directory: Option<SwarmName>,

        /// Local port to bind the bridge on when a bridge is picked (default: an
        /// ephemeral port).
        #[arg(long)]
        port: Option<u16>,

        /// Which lookup mechanisms reach the directory (same flags as
        /// `discover`): must match the advertiser's. Naming none (or `--public`)
        /// is the all-on public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,

        /// Password for a password-protected pick (🔒 in the picker) — prompts
        /// on pick when omitted.
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
