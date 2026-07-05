//! The blob consumer: redeem a `💬` ticket by dialing the producer's blob
//! endpoint, presenting the bearer secret, and streaming the content-addressed
//! bytes to a writer while verifying them against the ticket's SHA-256. Constant
//! memory — nothing is buffered whole. Standalone: it builds its own participant
//! endpoint and never touches the daemon.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use iroh::Endpoint;
use iroh::endpoint::Connection;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWrite, AsyncWriteExt as _};

use crate::lookup::{add_peer_addr, build_participant_endpoint};
use crate::protocol::crypto::{Password, TicketAuth};
use crate::util::consts::MAX_BLOB_BYTES;

use super::ticket::BlobTicket;
use super::{BLOB_ALPN, HASH_LEN, SECRET_LEN};

/// How long the first dial keeps retrying while the producer's address
/// propagates through the lookups (mirrors the a2a bridge).
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);
const RETRY_DELAY: Duration = Duration::from_secs(3);
const CHUNK: usize = 64 * 1024;

/// Fetch the blob named by `ticket`, streaming its bytes to `out` and verifying
/// the SHA-256 as they arrive. On a hash mismatch this returns an error *after*
/// bytes have already been written — the caller (e.g. `agent-gossip a2a fetch`) must
/// treat a non-zero result as "discard what landed".
///
/// # Errors
/// A password mismatch with the ticket, dial/handshake failure, the producer
/// refusing (bad secret / unknown blob), a size disagreement, a truncated
/// stream, or a final digest that does not match the ticket.
pub(crate) async fn fetch<W>(
    ticket: &BlobTicket,
    out: &mut W,
    password: Option<Password>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let token = auth_token(ticket, password.as_ref())?;
    let endpoint = build_participant_endpoint(&ticket.lookups).await?;
    add_peer_addr(&endpoint, ticket.addr.clone())?;
    let result = fetch_over(&endpoint, ticket, &token, out).await;
    endpoint.close().await;
    result
}

async fn fetch_over<W>(
    endpoint: &Endpoint,
    ticket: &BlobTicket,
    token: &[u8; SECRET_LEN],
    out: &mut W,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let conn = dial(endpoint, ticket).await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    // Request: sha256 ‖ secret, then done sending.
    send.write_all(&ticket.sha256).await?;
    send.write_all(token).await?;
    let _ = send.finish();

    // Response: size(u64) ‖ <size bytes>. A refusal closes the connection with a
    // coded reason before the size arrives.
    let mut size_buf = [0u8; 8];
    if recv.read_exact(&mut size_buf).await.is_err() {
        return Err(refusal(&conn));
    }
    let size = u64::from_le_bytes(size_buf);
    if size != ticket.size {
        bail!(
            "producer offered {size} bytes but the ticket promises {}",
            ticket.size
        );
    }
    if size > MAX_BLOB_BYTES {
        bail!("blob size {size} exceeds the maximum");
    }

    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
        let read = match recv.read(&mut buf[..want]).await? {
            Some(read) if read > 0 => read,
            _ => bail!("blob stream ended {remaining} bytes early"),
        };
        hasher.update(&buf[..read]);
        out.write_all(&buf[..read]).await?;
        remaining -= read as u64;
    }
    out.flush().await?;

    let digest = hasher.finalize();
    if digest.as_slice() != ticket.sha256 {
        bail!("content hash mismatch — the fetched bytes are not what the ticket promised");
    }
    Ok(())
}

/// The 32-byte auth token opening the fetch stream: the raw secret, or its
/// Argon2id stretch when the ticket is password-protected.
fn auth_token(ticket: &BlobTicket, password: Option<&Password>) -> Result<[u8; SECRET_LEN]> {
    match (ticket.password, password) {
        (true, Some(pw)) => Ok(TicketAuth::blob(&ticket.secret, Some(pw)).token),
        (true, None) => bail!("this blob ticket is password-protected — pass --password"),
        (false, Some(_)) => bail!("this blob ticket has no password — drop --password"),
        (false, None) => Ok(ticket.secret),
    }
}

/// Dial the producer's blob endpoint, retrying until [`DISCOVERY_DEADLINE`] so a
/// producer whose address is still propagating is waited for, not failed on.
async fn dial(endpoint: &Endpoint, ticket: &BlobTicket) -> Result<Connection> {
    let started = Instant::now();
    loop {
        match endpoint.connect(ticket.addr.clone(), BLOB_ALPN).await {
            Ok(conn) => return Ok(conn),
            Err(error) if started.elapsed() < DISCOVERY_DEADLINE => {
                tracing::debug!(%error, "blob dial retrying while the address propagates");
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(error) => bail!("connecting to the blob producer failed: {error}"),
        }
    }
}

/// Turn a producer-side connection close into a readable error — the coded
/// close (`bad secret` / `unknown blob`) is surfaced to the user.
fn refusal(conn: &Connection) -> anyhow::Error {
    match conn.close_reason() {
        Some(reason) => anyhow!("the blob producer refused the fetch: {reason}"),
        None => anyhow!("the blob fetch ended before any data arrived"),
    }
}

/// Assert at compile time that the request header the producer reads
/// (`HASH_LEN + SECRET_LEN`) matches what this side writes.
const _: () = assert!(HASH_LEN + SECRET_LEN == 64);
