use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use iroh::Watcher;
use serde::Serialize;

use crate::protocol::swarm::LookupOpts;

use super::build_participant_endpoint;

/// This machine's network capability, read from iroh's net-report — the data
/// behind `doctor`'s Network section and its hole-punch verdict. The
/// net-report fields are `Option` because the endpoint can bind locally even
/// when no report completes (offline / slow): then only `node_id` /
/// `local_addrs` are known and the rest stay `None`.
#[derive(Debug, Serialize)]
pub(crate) struct NetworkCapability {
    pub node_id: String,
    pub local_addrs: Vec<SocketAddr>,
    /// `false` ⇒ no net-report finished within the probe budget; the fields
    /// below are then all `None` (we can't distinguish "unknown" from "no").
    pub report_obtained: bool,
    pub udp_v4: Option<bool>,
    pub udp_v6: Option<bool>,
    /// The hole-punch signal: `Some(false)` ⇒ endpoint-independent NAT (direct
    /// hole punching works); `Some(true)` ⇒ address-dependent (symmetric) NAT
    /// (direct punch unlikely, traffic relays); `None` ⇒ insufficient probes.
    pub mapping_varies_by_dest: Option<bool>,
    pub public_v4: Option<String>,
    pub public_v6: Option<String>,
    pub preferred_relay: Option<String>,
    pub preferred_relay_latency_ms: Option<u64>,
    pub captive_portal: Option<bool>,
}

/// Build a public participant endpoint and read iroh's net-report, bounded by
/// `timeout`. Bind failure is the only hard error; a net-report that never
/// completes (offline) still yields a `NetworkCapability` with the local bind
/// info and `report_obtained = false`.
///
/// # Errors
/// The endpoint fails to bind (the local network stack is broken).
pub(crate) async fn probe(timeout: Duration) -> Result<NetworkCapability> {
    let endpoint = build_participant_endpoint(&LookupOpts::public_preset()).await?;
    let node_id = endpoint.id().to_string();
    let local_addrs = endpoint.bound_sockets();

    // The endpoint runs net-report continuously; `initialized()` resolves on
    // the first completed report. Offline, it never resolves → we time out and
    // report only the local bind.
    let mut watcher = endpoint.net_report();
    let report = tokio::time::timeout(timeout, watcher.initialized())
        .await
        .ok();

    let capability = match report {
        Some(report) => {
            let preferred = report.preferred_relay.clone();
            let preferred_relay_latency_ms = preferred.as_ref().and_then(|url| {
                report
                    .relay_latency
                    .iter()
                    .filter(|(_, relay, _)| *relay == url)
                    .map(|(_, _, latency)| latency)
                    .min()
                    .map(|latency| u64::try_from(latency.as_millis()).unwrap_or(u64::MAX))
            });
            NetworkCapability {
                node_id,
                local_addrs,
                report_obtained: true,
                udp_v4: Some(report.udp_v4),
                udp_v6: Some(report.udp_v6),
                mapping_varies_by_dest: report.mapping_varies_by_dest(),
                public_v4: report.global_v4.map(|addr| addr.to_string()),
                public_v6: report.global_v6.map(|addr| addr.to_string()),
                preferred_relay: preferred.map(|url| url.to_string()),
                preferred_relay_latency_ms,
                captive_portal: report.captive_portal,
            }
        }
        None => NetworkCapability {
            node_id,
            local_addrs,
            report_obtained: false,
            udp_v4: None,
            udp_v6: None,
            mapping_varies_by_dest: None,
            public_v4: None,
            public_v6: None,
            preferred_relay: None,
            preferred_relay_latency_ms: None,
            captive_portal: None,
        },
    };
    endpoint.close().await;
    Ok(capability)
}
