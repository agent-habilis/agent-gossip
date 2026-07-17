//! End-to-end smoke test: pipe a known payload through `mesh-pipe listen` and
//! read it back out of `mesh-pipe connect`, over a real loopback gossip mesh
//! (two subprocesses of the built binary). Proves the non-a2a `App`-frame path
//! carries raw bytes across the mesh intact.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// Pull the minted `💬…` id out of the sender's stderr line
/// (`mesh-pipe: mesh 💬…`).
async fn read_mesh_id(reader: &mut BufReader<tokio::process::ChildStderr>) -> Option<String> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 {
            return None;
        }
        if let Some(rest) = line.trim().strip_prefix("mesh-pipe: mesh ") {
            return Some(rest.trim().to_owned());
        }
    }
}

async fn reap(mut child: Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn bytes_roundtrip_over_the_mesh() {
    let bin = env!("CARGO_BIN_EXE_mesh-pipe");
    let payload = "hello over the gossip mesh — 💬 line one\nand line two\n";

    // Sender: create a loopback mesh (bare `listen`), print its id, read stdin.
    let mut listen = Command::new(bin)
        .arg("listen")
        .arg("--nick")
        .arg("sender")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn listen");

    let mut listen_stderr = BufReader::new(listen.stderr.take().expect("listen stderr"));
    let mesh_id = tokio::time::timeout(Duration::from_secs(15), read_mesh_id(&mut listen_stderr))
        .await
        .expect("timed out waiting for the mesh id")
        .expect("sender printed a mesh id");

    // Receiver: join that id, stream inbound bytes to its stdout.
    let mut connect = Command::new(bin)
        .arg("connect")
        .arg(&mesh_id)
        .arg("--nick")
        .arg("receiver")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn connect");
    let mut connect_stdout = connect.stdout.take().expect("connect stdout");

    // Let the two peers form the loopback mesh before the payload is written
    // (the sender buffers until meshed anyway, but this keeps the timing clear).
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Feed the payload, then close stdin so the sender emits `pipe_eof`.
    {
        let mut listen_stdin = listen.stdin.take().expect("listen stdin");
        listen_stdin
            .write_all(payload.as_bytes())
            .await
            .expect("write payload");
        listen_stdin.flush().await.expect("flush payload");
        // Dropped here → EOF on the sender's stdin.
    }

    // The receiver exits on `pipe_eof`, closing its stdout; read it to the end.
    let mut received = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(20),
        connect_stdout.read_to_end(&mut received),
    )
    .await;

    reap(listen).await;
    reap(connect).await;

    read.expect("timed out reading the receiver's stdout")
        .expect("read the receiver's stdout");
    assert_eq!(
        String::from_utf8_lossy(&received),
        payload,
        "the bytes piped into the sender must come out of the receiver unchanged"
    );
}
