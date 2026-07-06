//! Why the spool is a *mirror*, not a transport lane: broadcasts already ride
//! gossip, so it only duplicates the **durable** outbound stream to disk and
//! feeds foreign files back through the same `gossip::ingest` seam. Frames are
//! content-addressed, so a re-copied or event-coalesced file ingests at most
//! once; ephemeral plumbing (presence, digests, …) is never written, so an
//! ingested file can't resurrect a departed peer.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use bytes::Bytes;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::Message;
use crate::protocol::identity::content_hash_hex;
use crate::util::consts::{
    MAX_MESSAGE_SIZE, SPOOL_CHANNEL_CAPACITY, SPOOL_RESCAN_INTERVAL_SECS, SPOOL_SWEEP_INTERVAL_SECS,
};
use crate::util::tuning::spool_max_bytes;

pub(crate) const LOG_TARGET: &str = "agent_square::spool";

/// Extension of a committed frame file. The writer's in-flight temp files use a
/// leading-dot `.tmp.<pid>` name instead, so the watcher's extension filter
/// never sees a half-written frame (nor a stray `.DS_Store`).
const FRAME_EXT: &str = "frame";

/// Bytes of the content hash kept in the filename (16 bytes = 128 bits of
/// collision resistance over one mesh's frames — far below any birthday risk,
/// and the file's bytes are re-verified on ingest regardless).
const HASH_HEX_CHARS: usize = 32;

/// Size gate before a file is read: a frame is one wire message, capped at
/// [`MAX_MESSAGE_SIZE`]. Doubled for slack so a foreign non-atomic writer's
/// slightly-oversize or still-copying file is read and parse-dropped on ingest
/// rather than silently skipped forever; a genuinely huge file is skipped
/// without a read.
const MAX_SPOOL_FILE_BYTES: u64 = 2 * MAX_MESSAGE_SIZE as u64;

/// Owner-only permissions. An unpassworded mesh's `.frame` bytes are plaintext
/// wire JSON, so the directory and its files must not be world-readable by
/// default — the same local-user leak the runtime dir was hardened against
/// (commit 2390f7e). A user who deliberately wants cross-user sharing
/// pre-creates the directory with looser perms; `create_dir_all` never relaxes
/// an existing directory.
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// A unit of work for the writer task: a frame to persist, or a flush barrier
/// that replies once every frame queued before it has hit disk.
enum WriterMsg {
    Frame(Bytes),
    Flush(oneshot::Sender<()>),
}

/// The write half: hands durable outbound frames to the writer task. Cloned
/// into the `MeshSender` so every `broadcast` mirrors to disk.
#[derive(Debug)]
pub(crate) struct SpoolWriter {
    tx: mpsc::Sender<WriterMsg>,
}

impl SpoolWriter {
    /// Mirror one frame to the spool — unless it is ephemeral plumbing
    /// ([`crate::protocol::MessageKind::is_spoolable`]), which is never
    /// persisted (a stale presence/dial-hint file would resurrect a departed
    /// peer when ingested later). Non-blocking: a full queue drops the frame
    /// with a warn (anti-entropy re-serves it — the same lossy contract as
    /// gossip). A frame that doesn't parse is not one of ours to mirror.
    pub(crate) fn write(&self, bytes: &Bytes) {
        if !Message::parse(bytes).is_ok_and(|message| message.kind.is_spoolable()) {
            return;
        }
        if let Err(error) = self.tx.try_send(WriterMsg::Frame(bytes.clone())) {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "spool write queue full or closed; dropping frame (anti-entropy re-serves)"
            );
        }
    }

    /// Wait until every frame queued before this call has been written to disk.
    /// The shutdown path awaits it (bounded by a timeout) so a burst-then-quit
    /// doesn't lose the tail — the exact frames a sneakernet hand-off needs.
    /// A blocking send (not `try_send`) so a full queue delays the barrier
    /// rather than skipping it; the caller bounds the total wait.
    pub(crate) async fn flush(&self) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(WriterMsg::Flush(reply_tx)).await.is_ok() {
            let _ = reply_rx.await;
        }
    }
}

/// A live spool: the writer handle, the inbound frame stream the event loop
/// drains into `gossip::ingest`, and the watcher guard. The watcher must stay
/// owned for the daemon's lifetime — dropping it stops inbound ingestion (and,
/// via the closed path channel, the scanner task).
pub(crate) struct Spool {
    pub(crate) writer: Arc<SpoolWriter>,
    pub(crate) inbound_rx: mpsc::Receiver<Bytes>,
    watcher: RecommendedWatcher,
}

impl std::fmt::Debug for Spool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Spool")
            .field("writer", &self.writer)
            .finish_non_exhaustive()
    }
}

impl Spool {
    /// Consume into (writer, inbound stream, watcher guard). The event loop
    /// wraps the writer into its [`crate::transport::MeshSender`], drains the
    /// inbound stream in a `select!` arm, and holds the watcher for its whole
    /// lifetime (dropping the watcher stops ingestion).
    pub(crate) fn into_parts(
        self,
    ) -> (Arc<SpoolWriter>, mpsc::Receiver<Bytes>, RecommendedWatcher) {
        (self.writer, self.inbound_rx, self.watcher)
    }
}

/// Create `<root>/<mesh-prefix>/` (owner-only), spawn the writer +
/// watcher/scanner tasks, and return the live [`Spool`]. Fails loudly if the
/// directory can't be created or the OS watcher can't be installed — a bad
/// `--spool` path should abort startup, not silently disable the mirror.
///
/// The subdir reuses [`crate::util::mesh_prefix`] (the same filesystem-safe
/// per-mesh stem the runtime dir uses) so every peer of a mesh agrees on the
/// path from the id alone.
pub(crate) fn install(root: &Path, mesh_id: &str) -> Result<Spool> {
    use std::os::unix::fs::DirBuilderExt as _;

    let dir = root.join(crate::util::mesh_prefix(mesh_id));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(&dir)
        .with_context(|| format!("creating spool directory {}", dir.display()))?;

    let (out_tx, out_rx) = mpsc::channel::<WriterMsg>(SPOOL_CHANNEL_CAPACITY);
    let (in_tx, in_rx) = mpsc::channel::<Bytes>(SPOOL_CHANNEL_CAPACITY);
    let (path_tx, path_rx) = mpsc::channel::<PathBuf>(SPOOL_CHANNEL_CAPACITY);

    tokio::spawn(writer_task(dir.clone(), out_rx));
    tokio::spawn(scanner_task(dir.clone(), path_rx, in_tx));

    // notify runs the callback on its OWN thread (never a tokio worker), so a
    // `blocking_send` here is safe and back-pressures the OS event source
    // rather than dropping paths; the scanner's periodic rescan is the safety
    // net for anything notify still coalesces or drops.
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else { return };
        if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            return;
        }
        for path in event.paths {
            if is_frame(&path) && path_tx.blocking_send(path).is_err() {
                return;
            }
        }
    })
    .context("installing filesystem watcher")?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching spool directory {}", dir.display()))?;

    tracing::info!(target: LOG_TARGET, dir = %dir.display(), "spool active");
    Ok(Spool {
        writer: Arc::new(SpoolWriter { tx: out_tx }),
        inbound_rx: in_rx,
        watcher,
    })
}

/// Writer + GC task: mirror each frame to disk, reply to flush barriers, and
/// periodically evict the oldest frames past the byte cap.
async fn writer_task(dir: PathBuf, mut rx: mpsc::Receiver<WriterMsg>) {
    let mut sweep = tokio::time::interval(Duration::from_secs(SPOOL_SWEEP_INTERVAL_SECS));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(WriterMsg::Frame(bytes)) => write_frame(&dir, &bytes).await,
                // In-order processing: reaching the barrier means every prior
                // frame was written, so the reply confirms the flush.
                Some(WriterMsg::Flush(reply)) => { let _ = reply.send(()); }
                None => break, // MeshSender dropped → daemon shutting down
            },
            _ = sweep.tick() => gc(&dir, spool_max_bytes()).await,
        }
    }
}

/// Write one frame as `<hash>.frame` via an owner-only temp file + atomic
/// rename. Skips the write if the final path already exists (idempotent
/// re-mirror, and benign overlap when several daemons share the directory).
async fn write_frame(dir: &Path, bytes: &Bytes) {
    let mut hash = content_hash_hex(bytes);
    hash.truncate(HASH_HEX_CHARS);
    let final_path = dir.join(format!("{hash}.{FRAME_EXT}"));
    if tokio::fs::try_exists(&final_path).await.unwrap_or(false) {
        return;
    }
    // The pid disambiguates concurrent writers sharing the directory so their
    // temp files never collide before the rename.
    let tmp_path = dir.join(format!(".{hash}.tmp.{}", std::process::id()));
    if let Err(error) = write_private(&tmp_path, bytes).await {
        tracing::warn!(target: LOG_TARGET, %error, "spool temp write failed");
        return;
    }
    if let Err(error) = tokio::fs::rename(&tmp_path, &final_path).await {
        tracing::warn!(target: LOG_TARGET, %error, "spool rename failed");
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }
}

/// Write `bytes` to `path` created with owner-only (0600) permissions, so a
/// frame is never briefly world-readable under the process umask.
async fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.flush().await
}

/// Sum the `.frame` sizes; if over `max_bytes`, delete oldest-mtime-first until
/// under it. A `NotFound` on delete is ignored — a concurrent daemon's GC
/// racing us to the same file is benign.
async fn gc(dir: &Path, max_bytes: u64) {
    let mut frames = match frame_stats(dir).await {
        Ok(frames) => frames,
        Err(error) => {
            tracing::warn!(target: LOG_TARGET, %error, "spool GC scan failed");
            return;
        }
    };
    let total: u64 = frames.iter().map(|frame| frame.len).sum();
    if total <= max_bytes {
        return;
    }
    // Oldest first: mtime ascending. A tie on mtime falls back to the path so
    // the order is deterministic.
    frames.sort_by(|left, right| {
        left.mtime
            .cmp(&right.mtime)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut over = total - max_bytes;
    for frame in frames {
        if over == 0 {
            break;
        }
        match tokio::fs::remove_file(&frame.path).await {
            Ok(()) => over = over.saturating_sub(frame.len),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                over = over.saturating_sub(frame.len);
            }
            Err(error) => tracing::warn!(target: LOG_TARGET, %error, "spool GC delete failed"),
        }
    }
}

struct FrameStat {
    path: PathBuf,
    len: u64,
    mtime: SystemTime,
}

async fn frame_stats(dir: &Path) -> std::io::Result<Vec<FrameStat>> {
    let mut out = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !is_frame(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.push(FrameStat {
            path,
            len: meta.len(),
            mtime,
        });
    }
    Ok(out)
}

/// Watcher-fed scanner: forwards each new/changed `.frame` file's bytes to the
/// event loop. Keyed on `(filename → mtime)` so an unchanged file is ingested
/// once, while a foreign writer's mid-copy file (whose mtime advances when the
/// copy completes) is re-read and re-forwarded — `gossip::ingest` dedups the
/// overlap. A startup scan catches sneakernet files already present; a periodic
/// full rescan is the safety net for events the OS coalesced or dropped.
async fn scanner_task(dir: PathBuf, mut path_rx: mpsc::Receiver<PathBuf>, tx: mpsc::Sender<Bytes>) {
    let mut seen: HashMap<OsString, SystemTime> = HashMap::new();
    if scan_all(&dir, &mut seen, &tx).await.is_err() {
        return;
    }
    let mut rescan = tokio::time::interval(Duration::from_secs(SPOOL_RESCAN_INTERVAL_SECS));
    rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    rescan.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            path = path_rx.recv() => match path {
                Some(path) => {
                    if forward_path(&path, &mut seen, &tx).await.is_err() {
                        return; // event loop dropped the receiver
                    }
                }
                None => break, // watcher dropped → daemon shutting down
            },
            _ = rescan.tick() => {
                if scan_all(&dir, &mut seen, &tx).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Full directory sweep: forward every new/changed frame, then replace `seen`
/// with the currently-present set so entries for GC-deleted files drop out
/// (bounding `seen` to the on-disk frame count, itself capped by GC).
async fn scan_all(
    dir: &Path,
    seen: &mut HashMap<OsString, SystemTime>,
    tx: &mpsc::Sender<Bytes>,
) -> Result<(), ()> {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Ok(()); // a transient readdir error is retried next rescan
    };
    let mut present: HashMap<OsString, SystemTime> = HashMap::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_frame(&path) {
            continue;
        }
        let Some(name) = path.file_name().map(OsString::from) else {
            continue;
        };
        let (mtime, read) = classify(&path, &name, seen).await;
        match read {
            FrameRead::New(bytes) => {
                tx.send(bytes).await.map_err(|_| ())?;
                present.insert(name, mtime);
            }
            FrameRead::Unchanged => {
                present.insert(name, mtime);
            }
            // Omit from `present` so the next rescan retries it (a foreign copy
            // still in flight, or a transient stat/read error).
            FrameRead::Skip => {}
        }
    }
    *seen = present;
    Ok(())
}

/// Event-driven single-file forward. Same `(name → mtime)` gate as [`scan_all`].
async fn forward_path(
    path: &Path,
    seen: &mut HashMap<OsString, SystemTime>,
    tx: &mpsc::Sender<Bytes>,
) -> Result<(), ()> {
    let Some(name) = path.file_name().map(OsString::from) else {
        return Ok(());
    };
    let (mtime, read) = classify(path, &name, seen).await;
    if let FrameRead::New(bytes) = read {
        tx.send(bytes).await.map_err(|_| ())?;
        seen.insert(name, mtime);
    }
    Ok(())
}

enum FrameRead {
    New(Bytes),
    Unchanged,
    Skip,
}

/// One `stat` of `path`, classified against `seen`: `New(bytes)` when it is
/// new-or-changed and read cleanly, `Unchanged` when already ingested at this
/// mtime, `Skip` when unreadable or oversize (retried on the next rescan). The
/// single `stat` here supplies both the mtime gate and the size gate — the
/// caller never stats the file again.
async fn classify(
    path: &Path,
    name: &OsString,
    seen: &HashMap<OsString, SystemTime>,
) -> (SystemTime, FrameRead) {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return (SystemTime::UNIX_EPOCH, FrameRead::Skip);
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if seen.get(name) == Some(&mtime) {
        return (mtime, FrameRead::Unchanged);
    }
    if meta.len() > MAX_SPOOL_FILE_BYTES {
        return (mtime, FrameRead::Skip);
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => (mtime, FrameRead::New(Bytes::from(bytes))),
        Err(_) => (mtime, FrameRead::Skip),
    }
}

fn is_frame(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some(FRAME_EXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_frame_only_matches_committed_frames() {
        assert!(is_frame(Path::new("/spool/abc123.frame")));
        // In-flight temp file: the pid is the "extension", not `frame`.
        assert!(!is_frame(Path::new("/spool/.abc123.tmp.4242")));
        // Foreign noise the watcher must ignore.
        assert!(!is_frame(Path::new("/spool/.DS_Store")));
        assert!(!is_frame(Path::new("/spool/notes.txt")));
        assert!(!is_frame(Path::new("/spool/frame"))); // no extension
    }

    #[tokio::test]
    async fn write_frame_is_content_addressed_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("spool-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let bytes = Bytes::from_static(b"a frame's wire bytes");

        write_frame(&dir, &bytes).await;
        let after_first: Vec<_> = frame_stats(&dir).await.unwrap();
        assert_eq!(after_first.len(), 1, "one committed .frame file");
        let mtime = after_first[0].mtime;

        // Re-mirroring the same bytes is a no-op: same content hash → same
        // path → skip-if-exists (mtime unchanged, no temp files left behind).
        write_frame(&dir, &bytes).await;
        let after_second: Vec<_> = frame_stats(&dir).await.unwrap();
        assert_eq!(after_second.len(), 1);
        assert_eq!(after_second[0].mtime, mtime, "existing file untouched");

        // No `.tmp` residue.
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name();
            assert!(
                name.to_string_lossy().ends_with(".frame"),
                "unexpected residue: {name:?}"
            );
        }
        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn write_frame_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("spool-perm-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        write_frame(&dir, &Bytes::from_static(b"secret plaintext")).await;
        let stat = frame_stats(&dir).await.unwrap();
        assert_eq!(stat.len(), 1);
        let mode = tokio::fs::metadata(&stat[0].path)
            .await
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, FILE_MODE, "frame files must be owner-only");
        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn gc_evicts_oldest_beyond_cap() {
        let dir = std::env::temp_dir().join(format!("spool-gc-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Three ~1 KiB frames; mtimes ordered by write order (oldest first).
        for index in 0u8..3 {
            let payload = vec![index; 1024];
            write_frame(&dir, &Bytes::from(payload)).await;
            // Space the mtimes so oldest-first ordering is unambiguous.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(frame_stats(&dir).await.unwrap().len(), 3);

        // Cap at ~2 KiB: the single oldest frame is evicted.
        gc(&dir, 2048).await;
        let remaining = frame_stats(&dir).await.unwrap();
        assert_eq!(remaining.len(), 2, "oldest evicted to fit the cap");

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
