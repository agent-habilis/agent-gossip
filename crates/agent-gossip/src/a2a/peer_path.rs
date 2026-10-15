use habilis_network::embed::EventLoopState;
use habilis_network::iroh::endpoint::TransportAddrUsage;
use habilis_network::iroh::{Endpoint, TransportAddr};
use habilis_network::protocol::Channel;
use tokio::sync::oneshot;

use crate::a2a::surfaced::unix_secs;

// Copied from `habilis-network-iroh-transport-util/src/lib.rs`, which
// habilis-network does not re-export.
const WEBRTC_TRANSPORT_ID: u64 = 0x5752_5443;
const MULTIHOP_TRANSPORT_ID: u64 = 0x6d68;
const GOSSIP_TRANSPORT_ID: u64 = 0x6773;

const KIND_ORDER: [&str; 6] = ["ip", "webrtc", "multihop", "gossip", "custom", "relay"];

// Built from the *active* addresses, not the one iroh selected: `remote_info`
// cannot say which is selected, so a pair with both a direct path and the
// relay live reads `ip+relay` rather than a guess.
pub(crate) fn path_label(active: &[TransportAddr]) -> String {
    let kinds: Vec<&str> = KIND_ORDER
        .into_iter()
        .filter(|kind| active.iter().any(|addr| kind_of(addr) == Some(kind)))
        .collect();
    if kinds.is_empty() {
        return "unknown".to_owned();
    }
    kinds.join("+")
}

fn kind_of(addr: &TransportAddr) -> Option<&'static str> {
    match addr {
        TransportAddr::Ip(_) => Some("ip"),
        TransportAddr::Relay(_) => Some("relay"),
        TransportAddr::Custom(custom) => Some(match custom.id() {
            WEBRTC_TRANSPORT_ID => "webrtc",
            MULTIHOP_TRANSPORT_ID => "multihop",
            GOSSIP_TRANSPORT_ID => "gossip",
            _ => "custom",
        }),
        _ => None,
    }
}

/// The `agent-gossip peers` response: the roster snapshot with a `path` added
/// to each row. `peer_count` is the *active* peers plus self, while the array
/// chains the quiet peers on after them so a peer that may still return stays
/// addressable — count `!quiet` entries, never `peers.len()`, for the live
/// peers without self.
///
/// The `remote_info` lookups run on a spawned task: each is a round trip to the
/// endpoint actor, and the event loop that holds `state` must not wait on them.
pub(crate) fn respond_with_paths(
    state: &EventLoopState,
    endpoint: &Endpoint,
    resp_tx: oneshot::Sender<String>,
) {
    let snapshot = state.roster_snapshot();
    let endpoints: Vec<_> = snapshot
        .peers
        .iter()
        .map(|entry| {
            (!entry.quiet)
                .then(|| state.peer_endpoint(&entry.nickname))
                .flatten()
        })
        .collect();
    let mut rows = match serde_json::to_value(&snapshot.peers) {
        Ok(serde_json::Value::Array(rows)) => rows,
        _ => Vec::new(),
    };
    let readers = state.doc(Channel::Meta).to_json();
    for row in &mut rows {
        let reader = row
            .get("nickname")
            .and_then(serde_json::Value::as_str)
            .and_then(|nickname| reader_row(&readers, nickname, unix_secs()));
        if let (Some(reader), Some(fields)) = (reader, row.as_object_mut()) {
            fields.insert("reader".to_owned(), reader);
        }
    }
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        for (row, peer) in rows.iter_mut().zip(endpoints) {
            let label = match peer {
                Some(id) => active_path(&endpoint, id).await,
                None => "unknown".to_owned(),
            };
            if let Some(fields) = row.as_object_mut() {
                fields.insert("path".to_owned(), label.into());
            }
        }
        let _ = resp_tx.send(
            serde_json::json!({
                "ok": true,
                "peers": rows,
                "peer_count": snapshot.count,
            })
            .to_string(),
        );
    });
}

fn reader_row(meta: &serde_json::Value, nickname: &str, now: u64) -> Option<serde_json::Value> {
    let reader = meta.get("peers")?.get(nickname)?.get("reader")?;
    let bell = reader.get("bell")?.as_bool()?;
    let last_poll = reader.get("last_poll")?.as_u64()?;
    Some(serde_json::json!({
        "bell": bell,
        "last_poll_secs_ago": now.saturating_sub(last_poll),
    }))
}

async fn active_path(endpoint: &Endpoint, peer: habilis_network::iroh::EndpointId) -> String {
    let Some(info) = endpoint.remote_info(peer).await else {
        return "unknown".to_owned();
    };
    let active: Vec<TransportAddr> = info
        .addrs()
        .filter(|addr| matches!(addr.usage(), TransportAddrUsage::Active))
        .map(|addr| addr.addr().clone())
        .collect();
    path_label(&active)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use habilis_network::iroh::{RelayUrl, TransportAddr};

    use super::{path_label, reader_row};

    fn ip() -> TransportAddr {
        TransportAddr::Ip(SocketAddr::from(([127, 0, 0, 1], 4433)))
    }

    fn relay() -> TransportAddr {
        TransportAddr::Relay(
            "https://relay.example"
                .parse::<RelayUrl>()
                .expect("valid url"),
        )
    }

    fn custom(id: u64) -> TransportAddr {
        TransportAddr::Custom((id, &[1_u8, 2, 3][..]).into())
    }

    #[test]
    fn labels_each_kind_of_active_address() {
        assert_eq!(path_label(&[ip()]), "ip");
        assert_eq!(path_label(&[relay()]), "relay");
        assert_eq!(path_label(&[custom(0x5752_5443)]), "webrtc");
        assert_eq!(path_label(&[custom(0x6d68)]), "multihop");
        assert_eq!(path_label(&[custom(0x6773)]), "gossip");
        assert_eq!(path_label(&[custom(7)]), "custom");
    }

    #[test]
    fn joins_several_kinds_in_a_fixed_order_without_repeats() {
        assert_eq!(path_label(&[relay(), ip(), ip()]), "ip+relay");
        assert_eq!(path_label(&[relay(), custom(0x5752_5443)]), "webrtc+relay");
    }

    #[test]
    fn reader_row_reports_seconds_since_the_last_poll() {
        let meta = serde_json::json!({
            "peers": { "ana": { "reader": { "bell": true, "last_poll": 940 } } }
        });
        assert_eq!(
            reader_row(&meta, "ana", 1000),
            Some(serde_json::json!({ "bell": true, "last_poll_secs_ago": 60 }))
        );
    }

    #[test]
    fn reader_row_is_absent_without_an_entry() {
        let meta = serde_json::json!({ "peers": { "ana": { "status": "idle" } } });
        assert_eq!(reader_row(&meta, "ana", 1000), None);
        assert_eq!(reader_row(&meta, "bo", 1000), None);
    }

    #[test]
    fn no_active_address_is_unknown() {
        assert_eq!(path_label(&[]), "unknown");
    }
}
