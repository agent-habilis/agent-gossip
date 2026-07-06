//! `ipc`: the unix-socket / named-pipe listener used by the CLI's
//! `msg` and `poll` subcommands to talk to a running `create` or
//! `join` daemon. (The MCP stdio server is a separate, consumer-side path.)

pub mod ipc;
pub(crate) mod sender;
pub(crate) mod spool;

pub use sender::MeshSender;

/// Which transports a session may use for a **directed** message, a per-session
/// property (an embedder can run two sessions with different policies). Consumed
/// by `unicast::deliver` via `EventLoopState::transport`; broadcasts ignore it
/// (they structurally ride gossip). Each flag is the positive of a hidden CLI
/// switch: `--no-unicast` / `--no-gossip-directed` / `--no-circuit`.
#[derive(Clone, Copy, Debug)]
pub struct TransportPolicy {
    /// Attempt the point-to-point unicast transport for a directed message.
    pub unicast: bool,
    /// Let gossip carry (or be the fallback for) a directed message. Off makes
    /// directed traffic unicast/circuit-only with no gossip fallback.
    pub gossip_directed: bool,
    /// Attempt the multi-hop circuit transport when there is no direct route.
    pub circuit: bool,
}

impl TransportPolicy {
    /// All transports enabled — the production default and every non-CLI path
    /// (embed / MCP) unless overridden.
    pub const DEFAULTS: Self = Self {
        unicast: true,
        gossip_directed: true,
        circuit: true,
    };
}

impl Default for TransportPolicy {
    fn default() -> Self {
        Self::DEFAULTS
    }
}
