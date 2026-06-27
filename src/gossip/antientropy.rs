//! Anti-entropy: periodic digest broadcast + gap-fill resend. Recovers
//! messages a node missed while partitioned/asleep/just-joined.
//!
//! The digest advertises a **window** of the message log: an inclusive
//! `[lo, hi]` timestamp range plus the compact ids the sender holds in it
//! (raw 16-byte UUIDs, Base58-packed, so ~10× more fit one gossip message
//! than the old 36-char strings). A log larger than one window is swept
//! across rounds via a rolling cursor; the `[lo, hi]` bounds let a receiver
//! re-send only **in-window** gaps, so advertising a sub-window never makes
//! peers perpetually re-broadcast the out-of-window remainder.

use std::collections::HashSet;

use bytes::Bytes;
use iroh_gossip::api::GossipSender;
use serde::{Deserialize, Serialize};

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::message_log::DigestWindow;
use crate::daemon::state::EventLoopState;
use crate::protocol::{Message, MessageBody, Nickname, SwarmId};
use crate::util::tuning::{ANTIENTROPY_DIGEST_WINDOW_IDS, antientropy_max_resend};

use super::broadcast_msg;

/// One window on the wire: its `[lo, hi]` ts bounds (`hi == i64::MAX` ⇒
/// open-ended) plus its ids packed as raw 16-byte UUIDs, Base58-encoded.
#[derive(Serialize, Deserialize)]
struct WireWindow {
    lo: i64,
    hi: i64,
    ids: String,
}

impl WireWindow {
    fn encode(window: &DigestWindow) -> Self {
        let mut packed = Vec::with_capacity(window.ids.len() * 16);
        for id in &window.ids {
            packed.extend_from_slice(id);
        }
        WireWindow {
            lo: window.lo,
            hi: window.hi,
            ids: bs58::encode(packed).into_string(),
        }
    }

    /// Decode the packed ids into a set, or `None` if the Base58 / length
    /// is malformed.
    fn decode_ids(&self) -> Option<HashSet<[u8; 16]>> {
        let raw = bs58::decode(&self.ids).into_vec().ok()?;
        if raw.len() % 16 != 0 {
            return None;
        }
        Some(
            raw.chunks_exact(16)
                .map(|chunk| {
                    let mut id = [0u8; 16];
                    id.copy_from_slice(chunk);
                    id
                })
                .collect(),
        )
    }
}

/// The on-the-wire digest body: up to two windows (open-ended newest +
/// rolling closed older).
#[derive(Serialize, Deserialize)]
struct DigestBody {
    windows: Vec<WireWindow>,
}

/// Broadcast an anti-entropy digest: an **open-ended newest** window (so
/// holders re-send every newer message we lack — reconnect recovery) plus,
/// when the log is larger than one window, a rolling **closed** older
/// window (deep interior reconcile, swept across rounds via
/// `digest_cursor`). A node that missed messages while
/// partitioned/asleep/just-joined recovers. Like `PeerInfo`, never logged.
pub(crate) async fn broadcast_digest(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    // No real peer has ever linked — a digest would broadcast into the
    // void (mirrors `tick_heal`/`tick_alive` no-peer guards).
    if !state.meshed {
        return;
    }
    let recent = ANTIENTROPY_DIGEST_WINDOW_IDS;
    let Some(newest) = state.message_log.recent_window(recent) else {
        return; // empty log
    };
    let mut windows = vec![WireWindow::encode(&newest)];

    let older_len = state.message_log.older_len(recent);
    if older_len == 0 {
        state.digest_cursor = 0;
    } else {
        let start = state.digest_cursor % older_len;
        if let Some(older) = state.message_log.older_window(recent, start, recent) {
            state.digest_cursor = (start + older.ids.len()) % older_len;
            windows.push(WireWindow::encode(&older));
        }
    }

    let total_ids: usize = windows.iter().map(|window| window.ids.len()).sum();
    let Ok(json) = serde_json::to_string(&DigestBody { windows }) else {
        return;
    };
    let Ok(body) = MessageBody::new(json) else {
        return;
    };
    tracing::trace!(ids = total_ids, "anti-entropy digest broadcast");
    broadcast_msg(
        sender,
        &Message::new_digest(swarm, author, body).signed(&state.identity),
    )
    .await;
}

/// Handle a received anti-entropy digest: for each advertised window,
/// re-broadcast our logged messages the sender lacks **within that
/// window** (open-ended newest ⇒ everything newer; closed older ⇒ that
/// slice only), newest-first, up to `antientropy_max_resend()` total.
/// Receivers that already have them drop the repeat (dedup); the sender
/// (and anyone else who missed them) recovers. Never logged.
pub(crate) async fn handle_digest(message: &Message, state: &EventLoopState, ctx: &HandlerCtx<'_>) {
    let Ok(body) = serde_json::from_str::<DigestBody>(message.body.as_str()) else {
        return;
    };
    let mut budget = antientropy_max_resend();
    let mut resent = 0usize;
    for window in &body.windows {
        if budget == 0 {
            break;
        }
        let Some(have) = window.decode_ids() else {
            continue;
        };
        for msg in state
            .message_log
            .missing_in_window(window.lo, window.hi, &have, budget)
        {
            if let Ok(bytes) = msg.serialize() {
                let _ = ctx.sender.broadcast(Bytes::from(bytes)).await;
                resent += 1;
                budget -= 1;
            }
        }
    }
    if resent > 0 {
        tracing::debug!(resent, "anti-entropy: resent messages a peer was missing");
    }
}

/// Broadcast a **state** anti-entropy digest. The state log is unbounded, so —
/// like the chat digest — it is advertised in **windows** rather than one flat
/// set that would overflow a gossip message past ~170 ids. A holder re-sends any
/// state event a peer's advertised window omits, so a cold/late joiner pulls the
/// full state log over several rounds. Broadcast whenever meshed — even with an
/// empty log, so a fresh joiner advertises its (empty) set and gets backfilled.
///
/// State needs **complete** history convergence (chat is content with recent
/// context), so the windowing adds two things the chat digest lacks: a member
/// whose set fits one window advertises it open at **both** ends
/// ([`bootstrap_window`](crate::daemon::state_log::StateLog::bootstrap_window)),
/// and on the `start == 0` older sweep the bottom is opened so events *below*
/// the member's oldest held event are pulled too. Together they guarantee a
/// joiner reconciles the whole log, not just the tail.
pub(crate) async fn broadcast_state_digest(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    if !state.meshed {
        return;
    }
    let recent = ANTIENTROPY_DIGEST_WINDOW_IDS;
    let windows = if state.state_log.len() <= recent {
        // Whole set fits one window: advertise it open at both ends so a holder
        // fills every gap below, inside, and above — the full-history bootstrap.
        state.state_digest_cursor = 0;
        vec![WireWindow::encode(&state.state_log.bootstrap_window())]
    } else {
        let Some(newest) = state.state_log.recent_window(recent) else {
            return; // unreachable (len > recent ⇒ non-empty), but stay total.
        };
        let mut windows = vec![WireWindow::encode(&newest)];
        let older_len = state.state_log.older_len(recent);
        let start = state.state_digest_cursor % older_len;
        if let Some(mut older) = state.state_log.older_window(recent, start, recent) {
            // Open the bottom on the lowest sweep so a peer missing events below
            // its oldest held event (e.g. it received a live patch before
            // backfilling history) still pulls them.
            if start == 0 {
                older.lo = i64::MIN;
            }
            state.state_digest_cursor = (start + older.ids.len()) % older_len;
            windows.push(WireWindow::encode(&older));
        }
        windows
    };
    let Ok(json) = serde_json::to_string(&DigestBody { windows }) else {
        return;
    };
    let Ok(body) = MessageBody::new(json) else {
        return;
    };
    broadcast_msg(
        sender,
        &Message::new_state_digest(swarm, author, body).signed(&state.identity),
    )
    .await;
}

/// Handle a received state digest: for each advertised window, re-broadcast our
/// held state events the sender lacks **within that window** (oldest-first, so a
/// catching-up joiner fills bottom-up), up to a **own** resend budget (separate
/// from the chat digest's, so a busy chat log can't starve state backfill).
///
/// Unlike the chat digest, the windows' `have` ids are **unioned** before the
/// diff. State events authored in a burst share a one-second timestamp, so the
/// recent (`[lo, MAX]`) and older (`[MIN, hi]`) windows' ranges overlap; a
/// per-window `have` would then treat an event the sender holds, but listed
/// under the *other* window, as missing and re-send it forever — wasting the
/// budget and starving the genuinely-missing tail. Unioning first means we only
/// ever resend what the sender truly lacks.
pub(crate) async fn handle_state_digest(
    message: &Message,
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    let Ok(body) = serde_json::from_str::<DigestBody>(message.body.as_str()) else {
        return;
    };
    let mut have: HashSet<[u8; 16]> = HashSet::new();
    for window in &body.windows {
        if let Some(ids) = window.decode_ids() {
            have.extend(ids);
        }
    }
    let mut budget = antientropy_max_resend();
    let mut resent = 0usize;
    for window in &body.windows {
        if budget == 0 {
            break;
        }
        for event in state
            .state_log
            .missing_in_window(window.lo, window.hi, &have, budget)
        {
            if let Ok(bytes) = event.serialize() {
                let _ = ctx.sender.broadcast(Bytes::from(bytes)).await;
                // Mark it sent so the next (overlapping) window doesn't re-send
                // it and waste budget — equal-timestamp ranges overlap heavily.
                have.insert(event.id.as_uuid_bytes());
                resent += 1;
                budget -= 1;
            }
        }
    }
    if resent > 0 {
        tracing::debug!(
            resent,
            "state anti-entropy: resent state events a peer lacked"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{ANTIENTROPY_DIGEST_WINDOW_IDS, DigestBody, WireWindow};
    use crate::daemon::message_log::{DigestWindow, MessageLog};
    use crate::protocol::{Message, MessageBody, Nickname, SwarmId};

    /// A full two-window digest must serialize within the gossip message
    /// cap — the regression guard for the former overflow (200 UUID
    /// *strings* ≈ 8 KB, ~2× over `MAX_MESSAGE_SIZE`, silently dropped by
    /// gossip).
    #[test]
    fn digest_fits_gossip_cap() {
        // Enough messages that both a newest and a rolling older window are
        // full (each `ANTIENTROPY_DIGEST_WINDOW_IDS` ids).
        let total = ANTIENTROPY_DIGEST_WINDOW_IDS * 3;
        let mut log = MessageLog::new(total);
        for index in 0..total {
            let mut message = Message::new_message(
                &SwarmId::from("ahswtest"),
                &Nickname::from("author"),
                MessageBody::from(format!("m{index}").as_str()),
            );
            message.timestamp = 1_700_000_000 + i64::try_from(index).unwrap();
            log.push(message);
        }
        let newest = log.recent_window(ANTIENTROPY_DIGEST_WINDOW_IDS).unwrap();
        let older = log
            .older_window(
                ANTIENTROPY_DIGEST_WINDOW_IDS,
                0,
                ANTIENTROPY_DIGEST_WINDOW_IDS,
            )
            .unwrap();
        let digest_body = DigestBody {
            windows: vec![WireWindow::encode(&newest), WireWindow::encode(&older)],
        };
        let json = serde_json::to_string(&digest_body).unwrap();
        let body = MessageBody::new(json).expect("digest body has no control chars");

        // Worst-case envelope: a realistically long `ahsw…` swarm id.
        let swarm = SwarmId::from(
            "ahsw6bLvZNPGxuqnsbaPVGwf277NyTp8cYPCMiBxXED8d6TyBZpDDzZADkKHL7tTB1EjFagbCXYZ",
        );
        let digest = Message::new_digest(&swarm, &Nickname::from("a-fairly-long-nickname"), body);
        let wire = digest.serialize().expect("serialize digest");
        assert!(
            wire.len() <= crate::util::consts::MAX_MESSAGE_SIZE,
            "digest is {} bytes, over the {}-byte gossip cap",
            wire.len(),
            crate::util::consts::MAX_MESSAGE_SIZE
        );
    }

    /// The packed-id wire codec must round-trip exactly — a regression here
    /// would silently break cross-node reconciliation.
    #[test]
    fn wire_window_round_trips_ids() {
        let ids: Vec<[u8; 16]> = (0..5u8).map(|seed| [seed; 16]).collect();
        let window = DigestWindow {
            lo: 1,
            hi: i64::MAX,
            ids: ids.clone(),
        };
        let wire = WireWindow::encode(&window);
        let decoded = wire.decode_ids().expect("valid window decodes");
        assert_eq!(decoded, ids.into_iter().collect::<HashSet<_>>());
    }

    /// A malformed digest body must decode to `None` (so `handle_digest`
    /// skips it) rather than panic or yield garbage ids.
    #[test]
    fn decode_ids_rejects_malformed() {
        // Not valid Base58 (`0`, `O`, `I`, `l`, space are outside the alphabet).
        let bad_alphabet = WireWindow {
            lo: 0,
            hi: 0,
            ids: "0OIl not base58".to_string(),
        };
        assert!(bad_alphabet.decode_ids().is_none(), "bad Base58 ⇒ None");

        // Valid Base58 but not a whole number of 16-byte ids (5 bytes).
        let odd_length = WireWindow {
            lo: 0,
            hi: 0,
            ids: bs58::encode([1u8; 5]).into_string(),
        };
        assert!(odd_length.decode_ids().is_none(), "non-16-multiple ⇒ None");

        // Empty is well-formed: zero ids.
        let empty = WireWindow {
            lo: 0,
            hi: 0,
            ids: String::new(),
        };
        assert_eq!(empty.decode_ids().map(|set| set.len()), Some(0));
    }

    /// A **state** digest over a large (unbounded) log must still fit one gossip
    /// message — the windowed body caps it at two `ANTIENTROPY_DIGEST_WINDOW_IDS`
    /// windows, the regression guard for the overflow that advertising the whole
    /// flat id set would cause once the log grows past ~170 events.
    #[test]
    fn state_digest_fits_gossip_cap() {
        use crate::daemon::state_log::StateLog;

        let swarm = SwarmId::from(
            "ahsw6bLvZNPGxuqnsbaPVGwf277NyTp8cYPCMiBxXED8d6TyBZpDDzZADkKHL7tTB1EjFagbCXYZ",
        );
        let author = Nickname::from("a-fairly-long-nickname");
        let mut log = StateLog::new();
        // Far more than one window's worth, so both the newest and a full older
        // window are populated — the worst case for digest size.
        let total = ANTIENTROPY_DIGEST_WINDOW_IDS * 4;
        for index in 0..total {
            let mut event = Message::new_state(
                &swarm,
                &author,
                MessageBody::from(format!("s{index}").as_str()),
            );
            event.timestamp = 1_700_000_000 + i64::try_from(index).unwrap();
            assert!(log.insert(event), "fill the log");
        }
        let recent = ANTIENTROPY_DIGEST_WINDOW_IDS;
        let newest = log.recent_window(recent).expect("non-empty");
        let older = log.older_window(recent, 0, recent).expect("older portion");
        let windows = vec![WireWindow::encode(&newest), WireWindow::encode(&older)];
        let json = serde_json::to_string(&DigestBody { windows }).expect("serialize body");
        let body = MessageBody::new(json).expect("digest body has no control chars");
        let digest = Message::new_state_digest(&swarm, &author, body);
        let wire = digest.serialize().expect("serialize state digest");
        assert!(
            wire.len() <= crate::util::consts::MAX_MESSAGE_SIZE,
            "windowed state digest is {} bytes, over the {}-byte gossip cap",
            wire.len(),
            crate::util::consts::MAX_MESSAGE_SIZE
        );
    }

    /// The full digest body survives a serde round-trip, preserving the
    /// open-ended (`i64::MAX`) upper bound that drives reconnect recovery.
    #[test]
    fn digest_body_serde_round_trips() {
        let body = DigestBody {
            windows: vec![
                WireWindow {
                    lo: 100,
                    hi: i64::MAX,
                    ids: bs58::encode([7u8; 16]).into_string(),
                },
                WireWindow {
                    lo: 10,
                    hi: 50,
                    // Two *distinct* 16-byte ids (identical halves would
                    // dedup to one in the decoded set).
                    ids: {
                        let mut raw = [0u8; 32];
                        raw[16..].fill(1);
                        bs58::encode(raw).into_string()
                    },
                },
            ],
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: DigestBody = serde_json::from_str(&json).unwrap();
        assert_eq!(back.windows.len(), 2);
        assert_eq!(back.windows[0].hi, i64::MAX, "open-ended bound preserved");
        assert_eq!(back.windows[0].decode_ids().unwrap().len(), 1);
        assert_eq!(back.windows[1].decode_ids().unwrap().len(), 2);
    }
}
