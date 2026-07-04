//! On-stream framing for a circuit hop: a length-prefixed **onion header**
//! followed by the raw **payload** that is spliced hop-to-hop.
//!
//! Wire: `[u32 LE header-len][onion header bytes][payload bytes to EOF]`. Each
//! hop reads and strips one header (its peeled layer already yields the next
//! hop's header), then copies the remaining bytes straight through — so the
//! payload never touches a hop in cleartext form it can interpret.

use anyhow::{Result, bail};
use iroh::endpoint::{RecvStream, SendStream};

use crate::util::consts::MAX_MESSAGE_SIZE;

/// Cap on a single onion header, bounding allocation against a peer that lies
/// about the length. Circuits are short (~3 hops) and each layer is small JSON,
/// so a generous few-KiB-per-hop bound over a handful of hops is ample.
pub(super) const MAX_ONION_BYTES: usize = 64 * 1024;

/// Cap on the spliced payload — one wire `Message` plus envelope headroom (the
/// terminal delivers it into the same inbox as a unicast frame).
pub(super) const MAX_PAYLOAD_BYTES: usize = MAX_MESSAGE_SIZE + 256;

/// Write `[len][header]` to a stream.
///
/// # Errors
/// Fails on a write error or a header longer than [`MAX_ONION_BYTES`].
pub(super) async fn write_header(send: &mut SendStream, header: &str) -> Result<()> {
    let bytes = header.as_bytes();
    if bytes.len() > MAX_ONION_BYTES {
        bail!("onion header too large: {}", bytes.len());
    }
    let len = u32::try_from(bytes.len()).expect("bounded by MAX_ONION_BYTES");
    send.write_all(&len.to_le_bytes()).await?;
    send.write_all(bytes).await?;
    Ok(())
}

/// Read one `[len][header]` off a stream, leaving it positioned at the payload.
///
/// # Errors
/// Fails on a short read, an over-long header, or non-UTF-8 bytes.
pub(super) async fn read_header(recv: &mut RecvStream) -> Result<String> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_ONION_BYTES {
        bail!("onion header too large: {len}");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(String::from_utf8(buf)?)
}
