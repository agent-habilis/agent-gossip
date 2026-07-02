//! The **gossip subsystem**: the message transport plane. The outbound
//! send plane is [`broadcast`] (broadcast/buffer, presence/`PeerInfo`,
//! the `/reply` stdin path); the inbound plane is [`recv`] (the
//! gossip-event pump, neighbor up/down bookkeeping, the per-message
//! router); anti-entropy and the healer are [`antientropy`] / [`heal`].
//! This module itself holds only the shared `conn_path` diagnostic and
//! re-exports the subsystem's public API. Membership/presentation lives
//! in `lifecycle`; this layer never touches the participant roster
//! directly — it calls into `lifecycle::observe` and dispatches by kind.

pub(crate) mod antientropy;
mod broadcast;
pub(crate) mod heal;
mod recv;

use iroh::endpoint::TransportAddrUsage;
use iroh::{Endpoint, EndpointId, RelayUrl, TransportAddr};

pub(crate) use broadcast::{
    TaskLeg, broadcast_message, broadcast_msg, broadcast_state_merge, broadcast_task,
    handle_session_request, handle_stdin_line,
};
pub(crate) use recv::{drain_dead_receiver, handle_gossip_event};

/// Snapshot the active transport path to `node_id`: a short label
/// (`direct` / `relay` / `mixed` / `unknown`) plus the relay URL when
/// one is in use. Point-in-time, not a watcher — iroh starts a fresh
/// link relayed and upgrades to direct after hole-punching, so a label
/// taken right at `NeighborUp` skews toward `relay`; the periodic
/// census reading is the representative one. Diagnostics only; other
/// iroh apps wanted this too (sendme #67/#112, psyche #586). See
/// docs/iroh-ecosystem-research.md.
pub(crate) async fn conn_path(
    endpoint: &Endpoint,
    node_id: EndpointId,
) -> (&'static str, Option<RelayUrl>) {
    let Some(info) = endpoint.remote_info(node_id).await else {
        return ("unknown", None);
    };
    let mut has_direct = false;
    let mut has_relay = false;
    let mut relay_url = None;
    for addr in info.addrs() {
        if !matches!(addr.usage(), TransportAddrUsage::Active) {
            continue;
        }
        // `TransportAddr` is `#[non_exhaustive]`, so a wildcard is
        // mandatory and the match can't be made exhaustive; only relay
        // vs direct matters here.
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "TransportAddr is #[non_exhaustive]"
        )]
        match addr.addr() {
            TransportAddr::Relay(url) => {
                has_relay = true;
                relay_url = Some(url.clone());
            }
            TransportAddr::Ip(_) => has_direct = true,
            _ => {}
        }
    }
    let label = match (has_direct, has_relay) {
        (true, true) => "mixed",
        (true, false) => "direct",
        (false, true) => "relay",
        (false, false) => "unknown",
    };
    (label, relay_url)
}
