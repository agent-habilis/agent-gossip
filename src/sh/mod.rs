//! `ahsw sh` — broadcast a live terminal to peers over a direct, off-gossip
//! QUIC connection. `sh listen` spawns `$SHELL` in a pseudo-terminal, prints the
//! viewers' `ahsw sh connect 🐝…` command on stdout, and streams the shell's
//! output to every attached viewer; `sh connect <ticket>` redeems the ticket and
//! renders the shell read-only (the viewer's keyboard never reaches the shell).
//! The ticket is a bearer capability (a random secret) carrying the producer's
//! address + the swarm's discovery config, so the viewer needs nothing but it.
//!
//! The spawned shell *is* the session: when the sharer ends it (`exit`, Ctrl-D,
//! or the child dies), the producer FIN-closes every viewer and exits — nothing
//! outlives the shell.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::Endpoint;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::protocol::swarm::{LookupOpts, Swarm};

mod consume;
mod produce;
mod term;
mod ticket;

pub(crate) use consume::connect;
pub(crate) use produce::listen;

/// ALPN for the shell protocol — its own protocol identity, distinct from the
/// pipe/port/file ALPNs, so a mismatched dial is rejected at the QUIC handshake.
pub(crate) const SH_ALPN: &[u8] = b"agent-habilis-swarm/sh/1";

/// Length of the bearer-capability secret carried in a shell ticket.
pub(crate) const SECRET_LEN: usize = 32;

/// Cap a single frame's payload so a hostile producer can't make a viewer
/// allocate unboundedly. A PTY chunk is at most a few KB; `16 MiB` is slack.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Frame tags on the wire (`tag(1) ‖ len(u32 LE) ‖ payload`). Producer → viewer
/// only; the viewer never writes frames back.
const TAG_DATA: u8 = 0;
const TAG_RESIZE: u8 = 1;

/// One decoded frame from the producer.
enum Frame {
    /// Raw PTY output — written verbatim to the viewer's terminal.
    Data(Vec<u8>),
    /// The source terminal's dimensions — the viewer bounds its scroll region to
    /// the height and fills the margin outside the `cols × rows` box.
    Resize { cols: u16, rows: u16 },
}

/// Encode a data frame (`tag ‖ len ‖ bytes`) for broadcasting to viewers.
fn encode_data(data: &[u8]) -> Vec<u8> {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(TAG_DATA);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Encode a resize frame carrying `cols`/`rows` as `u16` LE.
fn encode_resize(cols: u16, rows: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + 4);
    out.push(TAG_RESIZE);
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&cols.to_le_bytes());
    out.extend_from_slice(&rows.to_le_bytes());
    out
}

/// Read one frame, or `Ok(None)` when the stream ends (a clean FIN or any drop —
/// the viewer treats a closed stream as "the sharer left").
///
/// # Errors
/// A frame longer than [`MAX_FRAME_BYTES`], an unknown tag, or a malformed
/// resize payload.
async fn read_frame<R: AsyncRead + Unpin>(recv: &mut R) -> Result<Option<Frame>> {
    let mut tag = [0u8; 1];
    // First byte: EOF/any error here means the stream ended, not a protocol fault.
    if recv.read_exact(&mut tag).await.is_err() {
        return Ok(None);
    }
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        bail!("frame too large: {len} bytes");
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await?;
    match tag[0] {
        TAG_DATA => Ok(Some(Frame::Data(payload))),
        TAG_RESIZE => {
            if payload.len() != 4 {
                bail!("resize frame must be 4 bytes, got {}", payload.len());
            }
            let cols = u16::from_le_bytes([payload[0], payload[1]]);
            let rows = u16::from_le_bytes([payload[2], payload[3]]);
            Ok(Some(Frame::Resize { cols, rows }))
        }
        other => bail!("unknown frame tag: {other}"),
    }
}

/// Resolve a `--swarm` id to its discovery config (`None` ⇒ a public default),
/// so a shell session traverses the network the way that swarm's members do.
fn swarm_lookups(swarm: Option<&str>) -> Result<LookupOpts> {
    match swarm {
        Some(id) => Ok(id
            .parse::<Swarm>()
            .context("invalid --swarm id")?
            .lookups()
            .clone()),
        None => Ok(LookupOpts::public_preset()),
    }
}

/// Best-effort wait (≤5s) for the endpoint to publish reachable addresses, so a
/// freshly-printed ticket resolves immediately. Never blocks forever.
async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}

/// Present the producer's status and the viewers' ready-to-run command on
/// **stdout** — the producer's product (its stdout carries no data; that flows
/// over the network), and stderr stays errors-only. Human (default) is
/// cargo-style; `json` is the bare command for machines.
fn announce(json: bool, serving: &str, command: &str) {
    tracing::info!("sharing {serving}");
    if json {
        println!("{command}");
        return;
    }
    crate::util::output::status_out("Sharing", serving);
    crate::util::output::status_out("Connect", command);
}
