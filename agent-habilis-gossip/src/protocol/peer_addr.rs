//! `PeerInfo` address wire codec.
//!
//! A `PeerInfo` message body carries the author's iroh `EndpointAddr`
//! as JSON so peers can dial each other directly. This is mesh-
//! formation plumbing, distinct from the swarm-id codec
//! (`protocol::swarm`) — kept in its own module so the two never get
//! conflated.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use iroh::{EndpointAddr, EndpointId, RelayUrl};

/// Serialize an `EndpointAddr` to a JSON value for `PeerInfo` messages.
pub fn endpoint_addr_to_json(addr: &EndpointAddr) -> serde_json::Value {
    let ips: Vec<String> = addr.ip_addrs().map(ToString::to_string).collect();
    let relay: Option<String> = addr.relay_urls().next().map(ToString::to_string);
    serde_json::json!({
        "id": addr.id.to_string(),
        "ips": ips,
        "relay": relay,
    })
}

/// Deserialize an `EndpointAddr` from a JSON value produced by `endpoint_addr_to_json`.
/// # Errors
/// Returns an error if the inputs are invalid or the operation fails.
pub fn endpoint_addr_from_json(json: &serde_json::Value) -> Result<(EndpointId, EndpointAddr)> {
    let id_str = json["id"].as_str().context("missing id")?;
    let endpoint_id: EndpointId = id_str.parse().context("invalid EndpointId")?;
    let mut addr = EndpointAddr::new(endpoint_id);
    if let Some(ips) = json["ips"].as_array() {
        for ip in ips {
            if let Some(text) = ip.as_str()
                && let Ok(socket) = text.parse::<SocketAddr>()
            {
                addr = addr.with_ip_addr(socket);
            }
        }
    }
    if let Some(relay) = json["relay"].as_str()
        && let Ok(url) = relay.parse::<RelayUrl>()
    {
        addr = addr.with_relay_url(url);
    }
    Ok((endpoint_id, addr))
}
