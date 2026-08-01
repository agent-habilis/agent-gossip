//! Compiles the C programs under `examples/mesh-pipe-c` against this crate's
//! cdylib and runs them.
//!
//! Two things are being checked. `ffi_test` is the C-side behavioural suite
//! (create + join, shared-state convergence, broadcast, directed send). The two
//! roundtrip tests pipe bytes *between* the C program and the Rust `mesh-pipe`
//! binary in both directions — that is the guard on the deliberate duplication
//! between `src/pipe.rs` and `examples/mesh-pipe/src/main.rs`: if the tag names,
//! the base64 body, the frame classification or the chunk budget drift apart,
//! one of these two directions stops reproducing its input.
//!
//! Everything here skips (loudly) rather than fails when the host has no C
//! compiler, or when the `mesh-pipe` binary has not been built.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// `target/<profile>/` — where the cdylib and the `mesh-pipe` binary land, and
/// where the compiled C programs go.
///
/// Derived by walking up from the test binary (`target/<profile>/deps/<name>`)
/// rather than from an env var: `CARGO_BIN_EXE_*` is only defined for the owning
/// package's tests, and `mesh-pipe` belongs to another package.
fn artifact_dir() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary has a path");
    path.pop(); // the test binary itself
    path.pop(); // deps/
    path
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/agent-habilis-mesh-ffi/, whose grandparent is
    // the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two levels below the workspace root")
        .to_path_buf()
}

fn cdylib_path() -> PathBuf {
    let name = if cfg!(target_os = "macos") {
        "libagent_habilis_mesh_ffi.dylib"
    } else if cfg!(target_os = "windows") {
        "agent_habilis_mesh_ffi.dll"
    } else {
        "libagent_habilis_mesh_ffi.so"
    };
    artifact_dir().join(name)
}

/// Make sure the cdylib the C programs link against exists. `cargo test` builds
/// the lib target (both crate types in one rustc invocation), so this is a
/// fallback for a stale or partial target dir — e.g. a run right after
/// `cargo clean` of just this artifact.
fn ensure_cdylib() {
    if cdylib_path().exists() {
        return;
    }
    let built = std::process::Command::new("cargo")
        .args(["build", "-p", "agent-habilis-mesh-ffi"])
        .current_dir(repo_root())
        .status();
    assert!(
        matches!(built, Ok(status) if status.success()) && cdylib_path().exists(),
        "the cdylib at {} is missing and could not be built",
        cdylib_path().display()
    );
}

/// Compile one C program to `target/<profile>/<out_name>`. `None` (with a printed
/// reason) when the host has no usable C compiler — every caller then skips.
///
/// Each caller passes a distinct `out_name`: cargo runs these tests in parallel,
/// and two compilers writing one output path would race.
fn compile(source: &str, out_name: &str) -> Option<PathBuf> {
    ensure_cdylib();
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let source_path = repo_root().join("examples/mesh-pipe-c").join(source);
    let include = repo_root().join("crates/agent-habilis-mesh-ffi/include");
    let libdir = artifact_dir();
    let output = libdir.join(out_name);
    let rpath = format!("-Wl,-rpath,{}", libdir.display());

    // `-Werror` but not `-Wpedantic`: the Makefile turns pedantic on for the
    // developer, while this path runs on whatever compiler the host has, and a
    // pedantic diagnostic nobody can reproduce locally must not fail the suite.
    let outcome = std::process::Command::new(&compiler)
        .arg("-std=gnu11")
        .arg("-O1")
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", include.display()))
        .arg("-o")
        .arg(&output)
        .arg(&source_path)
        .arg(format!("-L{}", libdir.display()))
        .arg("-lagent_habilis_mesh_ffi")
        .arg(rpath)
        .status();

    match outcome {
        Ok(status) if status.success() => Some(output),
        Ok(status) => panic!("{compiler} failed to compile {source}: {status}"),
        Err(error) => {
            eprintln!("skipping: no usable C compiler ({compiler}): {error}");
            None
        }
    }
}

/// The Rust twin, or `None` when it has not been built. A workspace test run
/// always builds it (its own tests use `CARGO_BIN_EXE_mesh-pipe`), so this only
/// skips for a narrowly-scoped `cargo test -p agent-habilis-mesh-ffi`.
fn mesh_pipe_bin() -> Option<PathBuf> {
    let path = artifact_dir().join("mesh-pipe");
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "skipping the cross-language roundtrip: {} is missing (run `cargo build -p mesh-pipe`)",
            path.display()
        );
        None
    }
}

/// Pull the minted id out of a sender's stderr. Both binaries print the same
/// `mesh-pipe: mesh <id>` line — that shared prefix is what makes them
/// interchangeable here.
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
        eprintln!("sender: {}", line.trim_end());
    }
}

async fn reap(mut child: Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// A payload that spans several frames (the budget is ~2 KB) and covers every
/// byte value, so a text-only or single-frame bug cannot pass.
fn payload() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(5000);
    while bytes.len() < 5000 {
        let next = u8::try_from(bytes.len() % 256).expect("a byte");
        bytes.push(next);
    }
    bytes.extend_from_slice("💬 tail marker\n".as_bytes());
    bytes
}

/// How the sender's stdin is wired. This is not a detail — it decides whether
/// the early-exit bug is even reachable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SenderStdin {
    /// A pipe the harness holds open and writes into once the receiver is up.
    /// Convenient, and it *masks* the bug: the sender sits in a read for as long
    /// as the test likes, so the mesh stays alive by accident.
    HeldPipe,
    /// A file, already at EOF when it is opened — how a person runs
    /// `listen < file`, and the shape that used to send and exit inside two
    /// seconds, before anybody could join.
    File,
}

/// One end-to-end case: pipe `payload()` from `sender listen` into
/// `receiver connect` and require it to come out unchanged.
struct Roundtrip<'a> {
    label: &'a str,
    sender: &'a Path,
    receiver: &'a Path,
    /// The C receiver's idle cap; empty for the Rust one, which has no such flag.
    extra_receiver_args: &'a [&'a str],
    stdin: SenderStdin,
    /// How long the receiver waits before joining. Non-zero is the regression:
    /// the sender must still be there when it arrives.
    receiver_delay: Duration,
}

async fn roundtrip(case: Roundtrip<'_>) {
    let expected = payload();

    // A file-backed sender needs its payload on disk before it starts; per-case
    // name because cargo runs these tests concurrently.
    let payload_path = artifact_dir().join(format!("{}-payload.bin", case.label));
    let stdin = match case.stdin {
        SenderStdin::HeldPipe => Stdio::piped(),
        SenderStdin::File => {
            std::fs::write(&payload_path, &expected).expect("write the payload file");
            let file = std::fs::File::open(&payload_path).expect("open the payload file");
            Stdio::from(file)
        }
    };

    let mut listen = Command::new(case.sender)
        .arg("listen")
        .args(["--nick", "sender"])
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the sender");

    let mut listen_stderr = BufReader::new(listen.stderr.take().expect("sender stderr"));
    let mesh_id = tokio::time::timeout(Duration::from_secs(30), read_mesh_id(&mut listen_stderr))
        .await
        .expect("timed out waiting for the mesh id")
        .expect("the sender printed a mesh id");

    // The regression: make the receiver show up *after* the sender is running.
    tokio::time::sleep(case.receiver_delay).await;

    let mut connect = Command::new(case.receiver)
        .arg("connect")
        .arg(&mesh_id)
        .args(["--nick", "receiver"])
        .args(case.extra_receiver_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the receiver");
    let mut connect_stdout = connect.stdout.take().expect("receiver stdout");

    if case.stdin == SenderStdin::HeldPipe {
        // Let the two peers form the loopback mesh before the payload goes out.
        // The sender buffers until meshed anyway; this keeps the timing legible.
        tokio::time::sleep(Duration::from_secs(5)).await;

        let mut listen_stdin = listen.stdin.take().expect("sender stdin");
        listen_stdin
            .write_all(&expected)
            .await
            .expect("write the payload");
        listen_stdin.flush().await.expect("flush the payload");
        // Dropped here → EOF on the sender's stdin → it emits the end-of-stream
        // marker → the receiver exits and closes its stdout.
    }
    // A file-backed sender needs no prodding: it is waiting for this receiver to
    // appear, and streams the file the moment it does.

    let mut arrived = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(45),
        connect_stdout.read_to_end(&mut arrived),
    )
    .await;

    reap(listen).await;
    reap(connect).await;

    read.expect("timed out reading the receiver's stdout")
        .expect("read the receiver's stdout");
    assert_eq!(
        arrived.len(),
        expected.len(),
        "the receiver produced {} bytes for a {}-byte payload",
        arrived.len(),
        expected.len()
    );
    assert!(
        arrived == expected,
        "the bytes changed in transit between {} and {}",
        case.sender.display(),
        case.receiver.display()
    );
}

#[test]
fn the_c_ffi_suite_passes() {
    let Some(program) = compile("ffi_test.c", "ffi_test") else {
        return;
    };
    let output = std::process::Command::new(&program)
        .output()
        .expect("run the C ffi_test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the C FFI suite failed ({}):\n{stdout}\n{stderr}",
        output.status
    );
    // Every scenario reports itself; a silent pass would mean it exited early.
    for expected in [
        "creates a mesh and two peers join it",
        "syncs the shared JSON state document both ways",
        "broadcasts a message to the gossip",
        "sends a message to one peer only",
    ] {
        assert!(
            stdout.contains(expected),
            "the C suite never reported {expected:?}:\n{stdout}"
        );
    }
}

/// The C receiver gives up after a minute of silence rather than hanging, so a
/// broken sender fails the test instead of the harness timeout. The Rust
/// receiver has no such flag.
const C_RECEIVER_ARGS: &[&str] = &["--idle-timeout", "60"];

#[tokio::test]
async fn bytes_roundtrip_from_c_to_rust() {
    let (Some(c_pipe), Some(rust_pipe)) = (compile("pipe.c", "pipe-c-to-rust"), mesh_pipe_bin())
    else {
        return;
    };
    roundtrip(Roundtrip {
        label: "c-to-rust",
        sender: &c_pipe,
        receiver: &rust_pipe,
        extra_receiver_args: &[],
        stdin: SenderStdin::HeldPipe,
        receiver_delay: Duration::ZERO,
    })
    .await;
}

#[tokio::test]
async fn bytes_roundtrip_from_rust_to_c() {
    let (Some(c_pipe), Some(rust_pipe)) = (compile("pipe.c", "pipe-rust-to-c"), mesh_pipe_bin())
    else {
        return;
    };
    roundtrip(Roundtrip {
        label: "rust-to-c",
        sender: &rust_pipe,
        receiver: &c_pipe,
        extra_receiver_args: C_RECEIVER_ARGS,
        stdin: SenderStdin::HeldPipe,
        receiver_delay: Duration::ZERO,
    })
    .await;
}

/// How long the receiver dawdles before joining in the two tests below. Long
/// enough that a sender which does not wait is already gone: before the fix the
/// whole process lived ~2s (C) / ~1s (Rust) with a file on stdin.
const LATE_JOIN_DELAY: Duration = Duration::from_secs(4);

/// `listen < file` used to send and exit before anyone could join, so the id it
/// printed was dead on arrival and the payload was lost with no error anywhere.
/// Both of these fail on that behaviour and pass once `listen` waits for a peer.
#[tokio::test]
async fn a_late_joiner_still_receives_a_file_backed_payload_from_c() {
    let (Some(c_pipe), Some(rust_pipe)) = (compile("pipe.c", "pipe-late-c"), mesh_pipe_bin())
    else {
        return;
    };
    roundtrip(Roundtrip {
        label: "late-c",
        sender: &c_pipe,
        receiver: &rust_pipe,
        extra_receiver_args: &[],
        stdin: SenderStdin::File,
        receiver_delay: LATE_JOIN_DELAY,
    })
    .await;
}

#[tokio::test]
async fn a_late_joiner_still_receives_a_file_backed_payload_from_rust() {
    let (Some(c_pipe), Some(rust_pipe)) = (compile("pipe.c", "pipe-late-rust"), mesh_pipe_bin())
    else {
        return;
    };
    roundtrip(Roundtrip {
        label: "late-rust",
        sender: &rust_pipe,
        receiver: &c_pipe,
        extra_receiver_args: C_RECEIVER_ARGS,
        stdin: SenderStdin::File,
        receiver_delay: LATE_JOIN_DELAY,
    })
    .await;
}
