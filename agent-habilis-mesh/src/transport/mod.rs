//! `ipc`: the unix-socket / named-pipe listener used by the CLI's
//! `msg` and `poll` subcommands to talk to a running `create` or
//! `join` daemon. (The MCP stdio server is a separate, consumer-side path.)

pub mod ipc;
pub(crate) mod sender;

pub use sender::MeshSender;

/// Which transports a session may use for a **directed** message, a per-session
/// property (an embedder can run two sessions with different policies). Consumed
/// by `unicast::deliver` via `EventLoopState::transport`; broadcasts ignore it
/// (they structurally ride gossip). Each flag is the positive of a hidden CLI
/// switch: `--no-unicast` / `--no-gossip-directed`.
///
/// Multi-hop reachability is no longer a per-message toggle: it is the
/// [`iroh-multihop-transport`](iroh_multihop_transport) custom transport
/// registered on the endpoint at build time (the `--multihop` flag). When
/// registered, a `unicast` connect transparently rides the multihop path if no
/// direct one exists — iroh's path selection decides.
#[derive(Clone, Copy, Debug)]
pub struct TransportPolicy {
    /// Attempt the point-to-point unicast transport for a directed message. iroh
    /// picks a direct path when available, else the multihop transport.
    pub unicast: bool,
    /// Let gossip carry (or be the fallback for) a directed message. Off makes
    /// directed traffic unicast-only with no gossip fallback.
    pub gossip_directed: bool,
}

impl TransportPolicy {
    /// All transports enabled — the production default and every non-CLI path
    /// (embed / MCP) unless overridden.
    pub const DEFAULTS: Self = Self {
        unicast: true,
        gossip_directed: true,
    };
}

impl Default for TransportPolicy {
    fn default() -> Self {
        Self::DEFAULTS
    }
}
