//! The forwarding **cell** — one opaque QUIC packet plus the source route it is
//! travelling — and its length-delimited framing over an underlay uni-stream.
//!
//! A cell carries the whole immutable forward `path` and a `pos` index marking
//! which hop should receive it, so a relay's only decision is "forward to
//! `path[pos + 1]`" or "deliver, I am the destination". `source` rides along so
//! the destination can build the return route without a fresh lookup.

use anyhow::{Context, Result, bail};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

use crate::addr::RouteHop;

/// A cell exceeding this many bytes is refused — a corrupt/hostile length prefix
/// must never drive an unbounded allocation. A QUIC datagram is a few kilobytes;
/// this leaves generous room for the route header.
const MAX_CELL_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Cell {
    /// The immutable forward route, `[first_hop, …, destination]`.
    pub(crate) path: Vec<RouteHop>,
    /// Index into `path` of the hop that should receive this cell.
    pub(crate) pos: u16,
    /// The original sender, so the destination can reverse the route to reply.
    pub(crate) source: RouteHop,
    /// The opaque QUIC packet being relayed.
    pub(crate) packet: Vec<u8>,
}

impl Cell {
    /// The next hop to forward to, or `None` when this hop is the destination.
    pub(crate) fn next_hop(&self) -> Option<&RouteHop> {
        self.path.get(self.pos as usize + 1)
    }

    /// Advance to the successor hop. Caller must have checked [`next_hop`] first.
    pub(crate) fn advanced(&self) -> Cell {
        Cell {
            path: self.path.clone(),
            pos: self.pos + 1,
            source: self.source.clone(),
            packet: self.packet.clone(),
        }
    }
}

/// Write one length-delimited cell: `[u32 big-endian len][postcard bytes]`.
pub(crate) async fn write_cell(send: &mut SendStream, cell: &Cell) -> Result<()> {
    let bytes = postcard::to_allocvec(cell).context("serialize cell")?;
    let len = u32::try_from(bytes.len()).context("cell too large to frame")?;
    send.write_all(&len.to_be_bytes())
        .await
        .context("write cell length")?;
    send.write_all(&bytes).await.context("write cell body")?;
    Ok(())
}

/// Read one length-delimited cell, refusing an oversized frame.
pub(crate) async fn read_cell(recv: &mut RecvStream) -> Result<Cell> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .context("read cell length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_CELL_BYTES {
        bail!("cell length {len} exceeds bound {MAX_CELL_BYTES}");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("read cell body")?;
    postcard::from_bytes(&buf).context("deserialize cell")
}
