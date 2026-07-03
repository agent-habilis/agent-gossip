use std::time::Duration;

use anyhow::{Result, bail};
use iroh::Endpoint;
use tokio::io::{AsyncRead, AsyncReadExt};

mod consume;
mod produce;
mod state_file;
mod term;
mod ticket;

pub(crate) use consume::connect;
pub(crate) use produce::listen;

/// Whether `ticket` decodes as a password-protected shell ticket — the CLI's
/// cue to prompt for a password before dialing. A malformed ticket ⇒ false; the
/// dial's own decode surfaces the real error.
pub(crate) fn ticket_requires_password(ticket: &str) -> bool {
    ticket::ShTicket::decode(ticket).is_ok_and(|decoded| decoded.password)
}

/// ALPN for the shell protocol — its own protocol identity, distinct from the
/// port/file ALPNs, so a mismatched dial is rejected at the QUIC handshake.
pub(crate) const SH_ALPN: &[u8] = b"agent-habilis-swarm/sh/1";

/// Env var injected into the broadcast shell (never read back by ahsw — the
/// no-env-config rule is about inbound config; this is informational, outbound
/// only). Its value is the session's sh-prefix — the first 16 chars of the
/// producer's endpoint id — which both marks "inside a swarm sh" for prompt
/// segments and keys the state file at [`state_file::path_for`], so a reader
/// derives the path from the env var alone.
pub(crate) const ENV_SH: &str = "AHSW_SH";

/// Length of the bearer-capability secret carried in a shell ticket.
pub(crate) const SECRET_LEN: usize = 32;

/// Cap a single frame's payload so a hostile producer can't make a viewer
/// allocate unboundedly. A PTY chunk is at most a few KB; `16 MiB` is slack.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Cap an input frame's payload well below [`MAX_FRAME_BYTES`]: keystrokes are
/// bytes and pastes are KBs, so anything larger is a protocol violation.
const MAX_INPUT_FRAME_BYTES: usize = 8 * 1024;

/// Frame tags on the wire (`tag(1) ‖ len(u32 LE) ‖ payload`). `DATA`/`RESIZE`
/// flow producer → viewer; `INPUT` flows viewer → producer, and only from a
/// write-capable viewer.
const TAG_DATA: u8 = 0;
const TAG_RESIZE: u8 = 1;
const TAG_INPUT: u8 = 2;

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

/// Encode an input frame (`tag ‖ len ‖ bytes`) carrying viewer keystrokes to
/// the producer.
fn encode_input(data: &[u8]) -> Vec<u8> {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(TAG_INPUT);
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

/// Read one input frame from a write-capable viewer, or `Ok(None)` when the
/// stream ends cleanly (the viewer FIN'd its send half, e.g. piped-stdin EOF).
/// Deliberately separate from [`read_frame`]: the producer accepts *only*
/// `INPUT`, so a viewer replaying producer frames is a protocol fault.
///
/// # Errors
/// Any tag other than [`TAG_INPUT`], or a payload longer than
/// [`MAX_INPUT_FRAME_BYTES`].
async fn read_input_frame<R: AsyncRead + Unpin>(recv: &mut R) -> Result<Option<Vec<u8>>> {
    let mut tag = [0u8; 1];
    // First byte: EOF/any error here means the stream ended, not a protocol fault.
    if recv.read_exact(&mut tag).await.is_err() {
        return Ok(None);
    }
    if tag[0] != TAG_INPUT {
        bail!("unexpected frame tag from viewer: {}", tag[0]);
    }
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_INPUT_FRAME_BYTES {
        bail!("input frame too large: {len} bytes");
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await?;
    Ok(Some(payload))
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};

    use crate::protocol::crypto::Password;
    use crate::protocol::swarm::LookupOpts;

    use super::ticket::ShTicket;
    use super::{Frame, MAX_INPUT_FRAME_BYTES, SH_ALPN, TAG_INPUT, encode_input, read_frame};

    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// Spawn a loopback producer serving `command`; returns its tickets and the
    /// server task (aborting it drops the endpoint and kills the PTY child).
    async fn spawn_producer(
        command: &str,
    ) -> (
        ShTicket,
        ShTicket,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        spawn_producer_pw(command, None).await
    }

    /// Like [`spawn_producer`] but protects the session with `password`, so its
    /// tickets carry the password flag and its tokens are the password stretch.
    async fn spawn_producer_pw(
        command: &str,
        password: Option<&Password>,
    ) -> (
        ShTicket,
        ShTicket,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        let (endpoint, ticket, write_ticket, secrets) =
            super::produce::bind(LookupOpts::loopback(), true, password)
                .await
                .expect("bind producer");
        let write_ticket = write_ticket.expect("write ticket");
        let command = command.to_owned();
        let server = tokio::spawn(async move {
            let sh_prefix: String = endpoint.id().to_string().chars().take(16).collect();
            let mut state_file = super::state_file::ShStateFile::new(
                std::env::temp_dir().join(format!("ahsw-sh-mod-test-{sh_prefix}.state.json")),
                &sh_prefix,
                None,
            );
            let result = super::produce::serve(
                &endpoint,
                secrets,
                Some(&command),
                Some(80),
                Some(24),
                false,
                &mut state_file,
            )
            .await;
            endpoint.close().await;
            result
        });
        (ticket, write_ticket, server)
    }

    /// Dial the producer as a viewer would: connect, open the bi-stream, present
    /// the ticket's secret. The endpoint is returned to keep it alive.
    async fn dial(ticket: &ShTicket) -> (iroh::Endpoint, Connection, SendStream, RecvStream) {
        let endpoint = crate::lookup::build_participant_endpoint(&ticket.lookups)
            .await
            .expect("build viewer endpoint");
        crate::lookup::add_peer_addr(&endpoint, ticket.addr.clone()).expect("register addr");
        let conn = endpoint
            .connect(ticket.addr.clone(), SH_ALPN)
            .await
            .expect("dial producer");
        let (mut send, recv) = conn.open_bi().await.expect("open bi-stream");
        send.write_all(&ticket.secret).await.expect("send secret");
        (endpoint, conn, send, recv)
    }

    /// Accumulate `Data` payloads until `needle` appears (panicking on a stream
    /// error or FIN before it does).
    async fn read_until(recv: &mut RecvStream, needle: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(TEST_TIMEOUT, read_frame(recv))
                .await
                .expect("frame before timeout")
                .expect("read frame")
            {
                Some(Frame::Data(bytes)) => {
                    out.extend_from_slice(&bytes);
                    if out.windows(needle.len()).any(|window| window == needle) {
                        return out;
                    }
                }
                Some(Frame::Resize { .. }) => {}
                None => panic!(
                    "stream ended before {:?} arrived",
                    String::from_utf8_lossy(needle)
                ),
            }
        }
    }

    /// Await the producer-initiated close and return its application error code.
    async fn closed_with_code(conn: &Connection) -> u64 {
        let reason = tokio::time::timeout(TEST_TIMEOUT, conn.closed())
            .await
            .expect("close before timeout");
        let ConnectionError::ApplicationClosed(app) = reason else {
            panic!("unexpected close reason: {reason:?}");
        };
        app.error_code.into_inner()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_viewer_input_reaches_the_shell_and_broadcasts() {
        // `read` consumes one line and the echo proves it traversed the PTY.
        let (read_ticket, write_ticket, server) =
            spawn_producer("read line; echo \"GOT-$line\"").await;

        let (_watch_ep, watch_conn, _watch_send, mut watch_recv) = dial(&read_ticket).await;
        let (_write_ep, write_conn, mut write_send, mut write_recv) = dial(&write_ticket).await;

        write_send
            .write_all(&encode_input(b"hello\n"))
            .await
            .expect("send input frame");

        // The shell's output reaches the writer and the read-only watcher alike.
        read_until(&mut write_recv, b"GOT-hello").await;
        read_until(&mut watch_recv, b"GOT-hello").await;

        watch_conn.close(0u32.into(), b"bye");
        write_conn.close(0u32.into(), b"bye");
        // `echo` exits after the line, so the producer winds down on PTY EOF.
        tokio::time::timeout(TEST_TIMEOUT, server)
            .await
            .expect("server exit before timeout")
            .expect("join server")
            .expect("serve");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_only_viewer_sending_input_is_disconnected() {
        let (read_ticket, _write_ticket, server) = spawn_producer("cat").await;
        let (_endpoint, conn, mut send, _recv) = dial(&read_ticket).await;

        send.write_all(&encode_input(b"rm -rf /\n"))
            .await
            .expect("send crafted input");

        assert_eq!(closed_with_code(&conn).await, 2);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_input_frame_is_disconnected() {
        let (_read_ticket, write_ticket, server) = spawn_producer("cat").await;
        let (_endpoint, conn, mut send, _recv) = dial(&write_ticket).await;

        // A header alone claiming an over-limit payload must trip the guard.
        let len = u32::try_from(MAX_INPUT_FRAME_BYTES + 1).unwrap();
        let mut frame = vec![TAG_INPUT];
        frame.extend_from_slice(&len.to_le_bytes());
        send.write_all(&frame).await.expect("send oversized header");

        assert_eq!(closed_with_code(&conn).await, 2);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wrong_secret_is_rejected() {
        let (read_ticket, _write_ticket, server) = spawn_producer("cat").await;
        let imposter = ShTicket {
            addr: read_ticket.addr.clone(),
            secret: [0xAA; super::SECRET_LEN],
            lookups: read_ticket.lookups.clone(),
            write: false,
            password: false,
        };
        let (_endpoint, conn, _send, _recv) = dial(&imposter).await;

        assert_eq!(closed_with_code(&conn).await, 1);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raw_secret_is_rejected_on_a_passworded_session() {
        // On a passworded session the expected token is the Argon2id stretch, so
        // a viewer that presents the ticket's raw secret (as a passwordless
        // client would) matches nothing and is closed.
        let password = Password::new("hunter2".to_owned());
        let (read_ticket, _write_ticket, server) =
            spawn_producer_pw("cat", Some(&password)).await;
        assert!(read_ticket.password, "the ticket must carry the password flag");

        // `dial` presents `read_ticket.secret` verbatim — the raw secret, not
        // the stretched token — which is exactly the attack we defend against.
        let (_endpoint, conn, _send, _recv) = dial(&read_ticket).await;

        assert_eq!(closed_with_code(&conn).await, 1);
        server.abort();
    }
}
