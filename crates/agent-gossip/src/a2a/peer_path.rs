use std::time::Duration;

use habilis_network::embed::EventLoopState;
use habilis_network::iroh::endpoint::TransportAddrUsage;
use habilis_network::iroh::{Endpoint, TransportAddr};
use habilis_network::net::{GOSSIP_TRANSPORT_ID, MULTIHOP_TRANSPORT_ID, WEBRTC_TRANSPORT_ID};
use habilis_network::ops::MeshSender;
use habilis_network::protocol::{MeshId, Nickname};
use tokio::sync::oneshot;
use tokio::time::Instant as TokioInstant;

use crate::a2a::app::{A2aApp, A2aResponder};
use crate::a2a::gossip_rpc::READER_GET;
use crate::a2a::send::{BroadcastA2aCallParams, broadcast_a2a_call};
use crate::a2a::tuning::POLL_WAITERS_CAP;

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
    readers: ReaderAsks,
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
        add_readers(&mut rows, readers).await;
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

/// How long `peers` waits for the answers to its reader queries, all together.
const READER_ASK_TIMEOUT: Duration = Duration::from_millis(500);

/// The reader queries `peers` sent, with the one deadline they all share.
pub(crate) struct ReaderAsks {
    pending: Vec<(Nickname, oneshot::Receiver<serde_json::Value>)>,
    deadline: TokioInstant,
}

/// The values `ask_readers` needs beyond its `state` and `app` handles.
pub(crate) struct ReaderAskParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) sender: &'a MeshSender,
}

/// Send `mesh/reader.get` to each live peer that has a known endpoint.
///
/// Real `a2a call`s share the waiter registry, so `peers` stops at half of it.
pub(crate) async fn ask_readers(
    params: ReaderAskParams<'_>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
) -> ReaderAsks {
    let ReaderAskParams {
        mesh,
        author,
        sender,
    } = params;
    let room = (POLL_WAITERS_CAP / 2).saturating_sub(app.a2a_waiters.len());
    let askable: Vec<Nickname> = state
        .roster_snapshot()
        .peers
        .iter()
        .filter(|entry| !entry.quiet && entry.nickname != *author)
        .filter(|entry| state.peer_endpoint(&entry.nickname).is_some())
        .take(room)
        .map(|entry| entry.nickname.clone())
        .collect();
    let deadline = TokioInstant::now() + READER_ASK_TIMEOUT;
    let mut pending = Vec::with_capacity(askable.len());
    for peer in askable {
        let (reply_tx, reply_rx) = oneshot::channel();
        broadcast_a2a_call(
            BroadcastA2aCallParams {
                mesh,
                author,
                peer: peer.clone(),
                method: READER_GET,
                params: serde_json::json!({}),
                timeout: READER_ASK_TIMEOUT,
                responder: A2aResponder::Typed(reply_tx),
                file: None,
            },
            state,
            app,
            sender,
        )
        .await;
        pending.push((peer, reply_rx));
    }
    ReaderAsks { pending, deadline }
}

/// All asks share one deadline, so one slow peer cannot delay `peers`. A peer
/// that misses it, or answers with an error, keeps its row with no `reader` key.
async fn add_readers(rows: &mut [serde_json::Value], asks: ReaderAsks) {
    let ReaderAsks { pending, deadline } = asks;
    for (peer, reply_rx) in pending {
        let Ok(Ok(reply)) = tokio::time::timeout_at(deadline, reply_rx).await else {
            continue;
        };
        let Some(reader) = reader_from_reply(&reply) else {
            continue;
        };
        let row = rows.iter_mut().find(|row| {
            row.get("nickname").and_then(serde_json::Value::as_str) == Some(peer.as_str())
        });
        if let Some(fields) = row.and_then(serde_json::Value::as_object_mut) {
            fields.insert("reader".to_owned(), reader);
        }
    }
}

/// The `reader` object of a row, from a peer's reply. An error reply (an older
/// daemon answers `-32601`) or a malformed result gives `None`.
fn reader_from_reply(reply: &serde_json::Value) -> Option<serde_json::Value> {
    let result = reply.get("result")?;
    let bell = result.get("bell")?.as_bool()?;
    let last_poll_secs_ago = result.get("last_poll_secs_ago")?;
    if !last_poll_secs_ago.is_null() && !last_poll_secs_ago.is_u64() {
        return None;
    }
    Some(serde_json::json!({ "bell": bell, "last_poll_secs_ago": last_poll_secs_ago }))
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

    use super::{
        GOSSIP_TRANSPORT_ID, MULTIHOP_TRANSPORT_ID, WEBRTC_TRANSPORT_ID, path_label,
        reader_from_reply,
    };

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
        assert_eq!(path_label(&[custom(WEBRTC_TRANSPORT_ID)]), "webrtc");
        assert_eq!(path_label(&[custom(MULTIHOP_TRANSPORT_ID)]), "multihop");
        assert_eq!(path_label(&[custom(GOSSIP_TRANSPORT_ID)]), "gossip");
        assert_eq!(path_label(&[custom(7)]), "custom");
    }

    #[test]
    fn joins_several_kinds_in_a_fixed_order_without_repeats() {
        assert_eq!(path_label(&[relay(), ip(), ip()]), "ip+relay");
        assert_eq!(
            path_label(&[relay(), custom(WEBRTC_TRANSPORT_ID)]),
            "webrtc+relay"
        );
    }

    #[test]
    fn a_reply_with_a_result_gives_a_reader() {
        let reply = serde_json::json!({ "result": { "bell": true, "last_poll_secs_ago": 7 } });
        assert_eq!(
            reader_from_reply(&reply),
            Some(serde_json::json!({ "bell": true, "last_poll_secs_ago": 7 }))
        );
        let before_any_poll =
            serde_json::json!({ "result": { "bell": false, "last_poll_secs_ago": null } });
        assert_eq!(
            reader_from_reply(&before_any_poll),
            Some(serde_json::json!({ "bell": false, "last_poll_secs_ago": null }))
        );
    }

    #[test]
    fn an_error_or_malformed_reply_gives_no_reader() {
        for reply in [
            serde_json::json!({ "error": { "code": -32601, "message": "method not found" } }),
            serde_json::json!({ "result": { "bell": "yes", "last_poll_secs_ago": 1 } }),
            serde_json::json!({ "result": { "bell": true } }),
            serde_json::json!({ "result": { "bell": true, "last_poll_secs_ago": -1 } }),
            serde_json::Value::Null,
        ] {
            assert_eq!(reader_from_reply(&reply), None, "{reply}");
        }
    }

    #[test]
    fn no_active_address_is_unknown() {
        assert_eq!(path_label(&[]), "unknown");
    }
}
