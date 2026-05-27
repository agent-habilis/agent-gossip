//! The lookup layer: building the iroh endpoint for a swarm mode and
//! wiring the selected lookups onto it. Each lookup mechanism lives in
//! its own submodule — [`mdns`] (LAN multicast), [`dht`] (mainline DHT),
//! and [`relay`] (the relay ladder + bootstrap-rung selection/failover).

mod dht;
mod mdns;
mod relay;

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey,
    endpoint::{PortmapperConfig, presets},
    protocol::Router,
};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};

use crate::protocol::swarm::{LookupOpts, RelayChoice};

pub(crate) use relay::{
    RungRefresh, plan_rung_refresh, relay_ladder, select_bootstrap_rung, spawn_relay_monitor,
};

/// Build an iroh endpoint for a swarm's lookups.
///
/// - `lookups`: which address-lookups (mDNS / DHT) and relay to wire.
///   When any lookup is on, the builder is composed from
///   `presets::Minimal` plus the selected lookups; the relay maps via
///   [`relay::relay_mode`]. An all-off (loopback-only) set wires none of
///   them.
/// - `secret_key`: `Some` pins a deterministic identity (used for the
///   shared rendezvous endpoint); `None` lets iroh generate a fresh
///   random key (the normal participant endpoint).
/// - `bind_port`: loopback-only — `Some(port)` binds
///   `127.0.0.1:port` (the deterministic rendezvous port; a bind
///   failure with `AddrInUse` is the claim-if-free signal that another
///   member already holds the beacon). `None` binds an ephemeral port.
///   Ignored when lookups are on (N0 manages binding).
pub(crate) async fn build_endpoint(
    lookups: &LookupOpts,
    secret_key: Option<SecretKey>,
    bind_port: Option<u16>,
) -> Result<Endpoint> {
    let is_beacon = secret_key.is_some();
    let network = lookups.network_label();
    let mut builder = if lookups.is_loopback() {
        debug_assert!(
            !lookups.mdns && !lookups.dht && lookups.relay == RelayChoice::Disabled,
            "loopback-only swarm must resolve to all-off lookups"
        );
        // Loopback-only = strictly loopback, **zero external network calls**.
        // `Minimal` picks the rustls crypto provider without N0's
        // DNS/relay defaults; we then lock down every path that could
        // touch a non-loopback host: `bind_addr` 127.0.0.1,
        // `RelayMode::Disabled` (no relay; no address-lookup is wired
        // for a loopback-only swarm so no DNS/pkarr/mDNS/DHT either), and
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
    } else {
        // `Minimal` (not `presets::N0`): N0-DNS is intentionally not
        // wired (the relay ladder is the fast path; DHT is the
        // operator-free eternal backstop). `Minimal` still sets the
        // rustls crypto provider. The mDNS / DHT address-lookups are
        // wired **after** bind (below) — in iroh 1.0 they live in
        // companion crates and need the bound endpoint's id.
        Endpoint::builder(presets::Minimal).relay_mode(relay::relay_mode(&lookups.relay))
    };

    if let Some(secret_key) = secret_key {
        builder = builder.secret_key(secret_key);
    }

    // Transport config is intentionally left at iroh's defaults: iroh tunes
    // keep-alive / idle (and the per-path multipath settings) for its
    // holepunching, and its own docs warn that adjusting them "may cause
    // suboptimal usage". A prior aggressive 10s idle / 5s keep-alive override
    // fought that tuning — marginal / distant links falsely idle-timed-out,
    // HyParView refilled from passive, and the resulting NeighborDown/Up churn
    // drove a per-connection memory leak. So we set nothing here.

    // For the private rendezvous endpoint this returns `AddrInUse`
    // when another member already holds the deterministic port — the
    // caller treats that as "someone else is the beacon" and retries.
    let endpoint = builder.bind().await.context("failed to bind endpoint")?;
    // Post-bind address-lookup wiring: in iroh 1.0 the mDNS / mainline-DHT
    // providers are companion crates built from the bound endpoint's id and
    // added to its lookup services. Loopback-only swarms wire none (asserted
    // above). The relay leg is configured pre-bind via `relay_mode`.
    if lookups.mdns {
        mdns::wire(&endpoint)?;
    }
    if lookups.dht {
        dht::wire(&endpoint)?;
    }
    tracing::info!(
        network,
        mdns = lookups.mdns,
        dht = lookups.dht,
        relay = ?lookups.relay,
        role = if is_beacon { "beacon" } else { "participant" },
        endpoint_id = %endpoint.id(),
        "endpoint bound"
    );
    Ok(endpoint)
}

/// The normal participant endpoint: a fresh random identity, no
/// pinned port. Thin intent-named wrapper over `build_endpoint`
/// so call sites don't carry the rendezvous-only `None, None`.
pub(crate) async fn build_participant_endpoint(lookups: &LookupOpts) -> Result<Endpoint> {
    build_endpoint(lookups, None, None).await
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
    //
    // A *failed* probe is the diagnostic signal a partition/post-sleep
    // re-bootstrap can't re-home the rendezvous, so it lands at `info`
    // (always-on file); a steady success every heal tick would be a
    // firehose, so it stays `debug`.
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if connected {
        tracing::debug!(connected, elapsed_ms, addr = ?addr, "rendezvous connect-probe finished");
    } else {
        tracing::info!(connected, elapsed_ms, addr = ?addr, "rendezvous connect-probe finished");
    }
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
    use super::{LookupOpts, build_participant_endpoint};

    // Binds the `Minimal`-based reachable branch (the default relay
    // ladder, no lookup wired) and the loopback all-off branch. mDNS
    // multicast / mainline-DHT socket setup is environment-dependent, so
    // it is not exercised here; presence-allowlist resolution is
    // unit-tested in `protocol::swarm`, and the relay ladder logic in
    // [`super::relay`].

    #[tokio::test]
    async fn loopback_all_off_binds() {
        let endpoint = build_participant_endpoint(&LookupOpts::loopback())
            .await
            .expect("loopback endpoint must bind");
        endpoint.close().await;
    }

    #[tokio::test]
    async fn public_default_relay_binds() {
        // No lookup wired: exercises the `Minimal` + pinned-ladder
        // composition. `bind()` is non-blocking wrt the relay, so this
        // is offline-safe even with the relay ladder configured.
        let endpoint = build_participant_endpoint(&LookupOpts::public_preset())
            .await
            .expect("endpoint with pinned relay ladder must bind");
        endpoint.close().await;
    }
}
