use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::address_lookup::{DhtAddressLookup, MdnsAddressLookup};
use iroh::{
    Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey,
    endpoint::{PortmapperConfig, default_relay_mode, presets},
    protocol::Router,
};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};

use crate::protocol::swarm::{DiscoveryOpts, SwarmMode};

/// The single relay every public member homes on by default. Pinning
/// **one** relay (vs iroh's 4-relay default, where members land on
/// different relays that do not cross-forward) gives every member a
/// guaranteed shared meeting point, and lets a cold joiner compute the
/// rendezvous address — `EndpointAddr::new(rendezvous_id)
/// .with_relay_url(this)` — with **zero address-lookup**, the
/// creator-independent analog of the pre-rewrite ticket's embedded
/// relay (the path that made public discovery instant).
///
/// iroh 0.98's `defaults::prod` NA-east relay (N0's *production*
/// hostname literally contains `iroh-canary` — it is the prod default,
/// not staging). Hardcoded, **not** `default_relay_mode()`, so members
/// on different iroh versions still pin the *same* relay. `--relay`
/// overrides it; if N0 ever retires this host the DHT address-lookup
/// is the operator-free eternal backstop (slower, still resolves).
const RENDEZVOUS_RELAY: &str = "https://use1-1.relay.n0.iroh-canary.iroh.link./";

/// Parsed once; `RelayUrl` clones are `Arc`-backed (cheap).
static RENDEZVOUS_RELAY_URL: LazyLock<RelayUrl> = LazyLock::new(|| {
    RENDEZVOUS_RELAY
        .parse()
        .expect("RENDEZVOUS_RELAY is a valid relay URL constant")
});

/// The relay the **beacon** homes on and the joiner pre-registers as
/// `rendezvous_id`'s address: the custom `--relay`, else the pinned
/// default. Single source of truth — the beacon leg of
/// [`build_endpoint_for_mode`] and `daemon::setup::register_rendezvous`
/// must agree or the relay-direct bootstrap dial finds nothing.
pub(crate) fn effective_public_relay(custom: Option<&RelayUrl>) -> RelayUrl {
    custom
        .cloned()
        .unwrap_or_else(|| RENDEZVOUS_RELAY_URL.clone())
}

/// Build an iroh endpoint for a swarm mode.
///
/// - `discovery`: which address-lookups (mDNS / DHT) and relay to
///   wire. For `Public` the builder is composed from `presets::Minimal`
///   plus the selected lookups; the relay defaults to the single
///   pinned [`RENDEZVOUS_RELAY`] (overridable via `--relay`). For
///   `Private` it must be all-off (the resolver guarantees this) —
///   loopback only.
/// - `secret_key`: `Some` pins a deterministic identity (used for the
///   shared rendezvous endpoint); `None` lets iroh generate a fresh
///   random key (the normal participant endpoint).
/// - `bind_port`: private mode only — `Some(port)` binds
///   `127.0.0.1:port` (the deterministic rendezvous port; a bind
///   failure with `AddrInUse` is the claim-if-free signal that another
///   member already holds the beacon). `None` binds an ephemeral port.
///   Ignored for public swarms (N0 manages binding).
pub(crate) async fn build_endpoint_for_mode(
    mode: SwarmMode,
    discovery: &DiscoveryOpts,
    secret_key: Option<SecretKey>,
    bind_port: Option<u16>,
) -> Result<Endpoint> {
    let is_beacon = secret_key.is_some();
    let network = mode.network_name();
    let mut builder = if mode == SwarmMode::Public {
        // `Minimal` (not `presets::N0`): N0-DNS is intentionally not
        // wired (the pinned relay is the fast path; DHT is the
        // operator-free eternal backstop). `Minimal` still sets the
        // rustls crypto provider.
        let mut builder = Endpoint::builder(presets::Minimal);
        if discovery.mdns {
            builder = builder.address_lookup(MdnsAddressLookup::builder());
            tracing::debug!("mDNS address-lookup wired (LAN multicast publish + resolve)");
        }
        if discovery.dht {
            builder = builder.address_lookup(DhtAddressLookup::builder());
            tracing::debug!("mainline DHT address-lookup wired (pkarr publish + resolve)");
        }
        // Asymmetric relay. The beacon (`secret_key.is_some()`) MUST
        // home on the one deterministic relay joiners pre-register, or
        // the relay-direct bootstrap dial finds nothing. The
        // participant uses resilient multi-relay `default_relay_mode()`
        // (a custom `--relay` overrides either): pinning the
        // participant to one relay made `bind()` block on that relay's
        // handshake (the 30s same-machine variance) and dropped iroh's
        // relay fallback — a `Default` participant still reaches the
        // beacon at its pinned relay and skips relays entirely
        // same-LAN via mDNS.
        let relay_mode = if secret_key.is_some() {
            let url = effective_public_relay(discovery.relay.as_ref());
            tracing::debug!(relay = %url, "beacon homes on the pinned rendezvous relay");
            RelayMode::custom([url])
        } else if let Some(url) = &discovery.relay {
            tracing::debug!(relay = %url, "participant pinned to custom --relay");
            RelayMode::custom([url.clone()])
        } else {
            tracing::debug!("participant on iroh resilient multi-relay default");
            default_relay_mode()
        };
        builder.relay_mode(relay_mode)
    } else {
        debug_assert!(
            !discovery.mdns && !discovery.dht && discovery.relay.is_none(),
            "private mode must resolve to all-off discovery (resolver invariant)"
        );
        // Private = strictly loopback, **zero external network calls**.
        // `Minimal` picks the rustls crypto provider without N0's
        // DNS/relay defaults; we then lock down every path that could
        // touch a non-loopback host: `bind_addr` 127.0.0.1,
        // `RelayMode::Disabled` (no relay; no address-lookup is wired
        // for private so no DNS/pkarr/mDNS/DHT either), and
        // `PortmapperConfig::Disabled` — the one remaining default-on
        // reach (UPnP/PCP/NAT-PMP to the LAN gateway, on even with the
        // relay off). With relay + portmapper off, iroh's netcheck has
        // no external targets (local-interface report only).
        // `bind_port` is the deterministic rendezvous port when
        // co-hosting the beacon, else 0 (ephemeral).
        Endpoint::builder(presets::Minimal)
            .bind_addr(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                bind_port.unwrap_or(0),
            ))
            .context("failed to set bind address")?
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
    };

    if let Some(secret_key) = secret_key {
        builder = builder.secret_key(secret_key);
    }

    // For the private rendezvous endpoint this returns `AddrInUse`
    // when another member already holds the deterministic port — the
    // caller treats that as "someone else is the beacon" and retries.
    let endpoint = builder.bind().await.context("failed to bind endpoint")?;
    tracing::info!(
        network,
        mdns = discovery.mdns,
        dht = discovery.dht,
        custom_relay = discovery.relay.is_some(),
        role = if is_beacon { "beacon" } else { "participant" },
        endpoint_id = %endpoint.id(),
        "endpoint bound"
    );
    Ok(endpoint)
}

/// The normal participant endpoint: a fresh random identity, no
/// pinned port. Thin intent-named wrapper over `build_endpoint_for_mode`
/// so call sites don't carry the rendezvous-only `None, None`.
pub(crate) async fn build_participant_endpoint(
    mode: SwarmMode,
    discovery: &DiscoveryOpts,
) -> Result<Endpoint> {
    build_endpoint_for_mode(mode, discovery, None, None).await
}

/// Register a peer's address so the endpoint can connect to it.
pub(crate) fn add_peer_addr(endpoint: &Endpoint, addr: EndpointAddr) -> Result<()> {
    let lookup = MemoryLookup::new();
    lookup.add_endpoint_info(addr);
    endpoint.address_lookup()?.add(lookup);
    tracing::debug!("registered a direct peer address with the endpoint");
    Ok(())
}

/// Bounded `GOSSIP_ALPN` connect-probe. Dialing forces iroh to
/// (re)resolve and (re)path `target` via the configured
/// address-lookups; the connection is only ever wanted for that side
/// effect. `true` iff a connection was established within `timeout`
/// (a foreign / dead / unreachable target yields `false`); callers
/// wanting only the resolution side effect ignore the bool.
pub(crate) async fn probe_connect(
    endpoint: &Endpoint,
    target: impl Into<EndpointAddr>,
    timeout: Duration,
) -> bool {
    let addr: EndpointAddr = target.into();
    let started = std::time::Instant::now();
    let connected = matches!(
        tokio::time::timeout(timeout, endpoint.connect(addr.clone(), GOSSIP_ALPN)).await,
        Ok(Ok(_conn))
    );
    // `?addr`: a loopback/private direct addr means a *local*
    // rendezvous co-host (self-partition signature); relay/public is
    // the cross-machine path. `elapsed_ms` exposes a slow relay
    // re-home outrunning the steady probe budget.
    tracing::debug!(
        connected,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        addr = ?addr,
        "rendezvous connect-probe finished"
    );
    connected
}

/// Build an iroh-gossip instance and a Router that accepts incoming gossip connections.
///
/// The Router spawns an accept loop that routes incoming QUIC connections
/// with the gossip ALPN to the Gossip protocol handler. Without this,
/// peers cannot accept inbound connections from other peers.
pub(crate) fn build_swarm(endpoint: Endpoint) -> (Gossip, Router) {
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let router = Router::builder(endpoint)
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();
    (gossip, router)
}

#[cfg(test)]
mod tests {
    use super::{DiscoveryOpts, SwarmMode, build_participant_endpoint};

    // Binds the `Minimal`-based public branch (the pinned relay, no
    // lookup wired) and the private all-off branch. mDNS multicast /
    // mainline-DHT socket setup is environment-dependent, so it is
    // not exercised here; presence-allowlist resolution is unit-tested
    // in `protocol::swarm`.

    #[tokio::test]
    async fn private_all_off_binds() {
        let disco = DiscoveryOpts {
            mdns: false,
            dht: false,
            relay: None,
        };
        let endpoint = build_participant_endpoint(SwarmMode::Private, &disco)
            .await
            .expect("private loopback endpoint must bind");
        endpoint.close().await;
    }

    #[tokio::test]
    async fn public_default_relay_binds() {
        // No lookup wired: exercises the `Minimal` + pinned-relay
        // composition. `bind()` is non-blocking wrt the relay, so this
        // is offline-safe even with the pinned relay configured.
        let disco = DiscoveryOpts {
            mdns: false,
            dht: false,
            relay: None,
        };
        let endpoint = build_participant_endpoint(SwarmMode::Public, &disco)
            .await
            .expect("public endpoint with pinned relay must bind");
        endpoint.close().await;
    }
}
