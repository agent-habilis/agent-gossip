//! OSC 9;4 terminal progress for `ahsw pipe`, written straight to `/dev/tty`
//! (the controlling terminal) so it never touches stdout (the pipe's data /
//! ticket) or stderr (errors only). With no controlling terminal (piped,
//! daemon, CI) the writes are skipped.
//!
//! OSC 9;4 is a terminal progress escape: `ESC ] 9 ; 4 ; state ; progress ST`
//! with state 1 = determinate (0..100), 2 = error, 3 = indeterminate, 0 = clear.
//! States 0 and 3 carry no `progress` value (the spec's canonical form).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const CHUNK: usize = 64 * 1024;

/// Drives a terminal's OSC 9;4 progress indicator. Every method is a no-op when
/// there is no controlling terminal to write to.
pub(crate) struct Progress {
    tty: Option<File>,
    total: Option<u64>,
    last_pct: Option<u8>,
    indeterminate_started: bool,
}

impl Progress {
    /// `total` is the stream's byte length when known (a determinate percent),
    /// else `None` (an indeterminate "loading" indicator). Emits the initial
    /// indicator immediately — callers build this the moment the peer connects,
    /// so feedback appears at connection time rather than after the first chunk.
    pub(crate) fn new(total: Option<u64>) -> Self {
        let mut progress = Self {
            tty: OpenOptions::new().write(true).open("/dev/tty").ok(),
            total,
            last_pct: None,
            indeterminate_started: false,
        };
        progress.start();
        progress
    }

    /// Show the indicator at 0% (determinate) or "loading" (indeterminate) right
    /// as the transfer begins, before any bytes flow.
    fn start(&mut self) {
        match self.total {
            Some(total) if total > 0 => {
                self.last_pct = Some(0);
                self.emit(1, 0);
            }
            Some(_) => {} // zero-length: nothing to show until finish().
            None => {
                self.indeterminate_started = true;
                self.emit(3, 0);
            }
        }
    }

    /// Report `done` bytes transferred. A determinate indicator emits only when
    /// the integer percent advances; an indeterminate one emits once.
    pub(crate) fn update(&mut self, done: u64) {
        match self.total {
            Some(total) if total > 0 => {
                let pct = pct(done, total);
                if self.last_pct != Some(pct) {
                    self.last_pct = Some(pct);
                    self.emit(1, pct);
                }
            }
            Some(_) => {} // zero-length stream: nothing to show until finish().
            None => {
                if !self.indeterminate_started {
                    self.indeterminate_started = true;
                    self.emit(3, 0);
                }
            }
        }
    }

    /// Keep-alive: re-emit the indeterminate "loading" state so the terminal
    /// doesn't fade it during a long stretch without updates. No-op for a
    /// determinate bar (its percent updates keep it alive).
    pub(crate) fn tick(&mut self) {
        if self.total.is_none() {
            self.indeterminate_started = true;
            self.emit(3, 0);
        }
    }

    /// Clear the indicator (successful completion).
    pub(crate) fn finish(&mut self) {
        self.emit(0, 0);
    }

    /// Flag the indicator as errored.
    pub(crate) fn fail(&mut self) {
        self.emit(2, 0);
    }

    fn emit(&mut self, state: u8, progress: u8) {
        if let Some(tty) = self.tty.as_mut() {
            // Best-effort: a write failure just drops the indicator.
            let _ = tty.write_all(osc(state, progress).as_bytes());
            let _ = tty.flush();
        }
    }
}

/// The OSC 9;4 escape for `(state, progress)`, ST-terminated (`ESC \`). Clear (0)
/// and indeterminate (3) take **no** progress value — the spec's canonical form,
/// and what Ghostty actually renders (a trailing `;0` leaves it invisible).
fn osc(state: u8, progress: u8) -> String {
    match state {
        0 | 3 => format!("\x1b]9;4;{state}\x1b\\"),
        _ => format!("\x1b]9;4;{state};{progress}\x1b\\"),
    }
}

/// Integer percent of `done`/`total`, clamped to `0..=100` (`u128` math so a
/// huge `total` can't overflow the `* 100`).
pub(crate) fn pct(done: u64, total: u64) -> u8 {
    let pct = u128::from(done.min(total)) * 100 / u128::from(total);
    u8::try_from(pct.min(100)).unwrap_or(100)
}

/// Copy buffer size. While throttled, use smaller chunks (~20/sec at the cap) so
/// progress updates often — a smooth bar even for files below the 64 `KiB` default.
pub(crate) fn throttle_chunk(throttle: Option<u64>) -> usize {
    match throttle {
        Some(rate) => usize::try_from((rate / 20).clamp(2048, CHUNK as u64)).unwrap_or(CHUNK),
        None => CHUNK,
    }
}

/// Sleep long enough that a `bytes`-sized chunk respects the `throttle` cap.
pub(crate) async fn pace(throttle: Option<u64>, bytes: usize) {
    if let Some(rate) = throttle {
        let micros = u64::try_from(bytes).unwrap_or(0) * 1_000_000 / rate;
        tokio::time::sleep(Duration::from_micros(micros)).await;
    }
}

/// Copy `reader` → `writer` in bounded chunks, pacing to `throttle` when set —
/// no progress (the producer's send path; its bar is driven by the consumer's
/// reverse progress reports). Each write is awaited, so QUIC backpressure holds.
///
/// # Errors
/// Propagates the first read/write error.
pub(crate) async fn copy_throttled<R, W>(
    reader: &mut R,
    writer: &mut W,
    throttle: Option<u64>,
) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    let mut done = 0u64;
    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return Err(error),
        };
        writer.write_all(&buf[..read]).await?;
        done += u64::try_from(read).unwrap_or(0);
        pace(throttle, read).await;
    }
    Ok(done)
}

/// Receive `reader` → `writer`, driving `progress`, and report the running
/// received-byte count upstream on `report` (one 8-byte LE write per integer-%
/// change, then a final count) so the sender's bar can track real delivery.
/// `throttle` caps throughput; `total` is the known length, for the % cadence.
///
/// # Errors
/// Propagates the first read/write error, flagging the indicator first.
pub(crate) async fn recv_with_reports<R, W, Rep>(
    reader: &mut R,
    writer: &mut W,
    report: &mut Rep,
    progress: &mut Progress,
    throttle: Option<u64>,
    total: Option<u64>,
) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Rep: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; throttle_chunk(throttle)];
    let mut done = 0u64;
    let mut last_pct = 0u8;
    // Keep the indeterminate bar alive while we wait on a slow sender (no-op once
    // determinate). `reader.read` is cancel-safe, so racing it loses no bytes.
    let mut keepalive = tokio::time::interval(Duration::from_millis(250));
    loop {
        let read = tokio::select! {
            result = reader.read(&mut buf) => match result {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    progress.fail();
                    return Err(error);
                }
            },
            _ = keepalive.tick() => {
                progress.tick();
                continue;
            }
        };
        if let Err(error) = writer.write_all(&buf[..read]).await {
            progress.fail();
            return Err(error);
        }
        done += u64::try_from(read).unwrap_or(0);
        pace(throttle, read).await;
        progress.update(done);
        if let Some(total) = total {
            let now = pct(done, total);
            if now != last_pct {
                last_pct = now;
                if let Err(error) = report.write_all(&done.to_le_bytes()).await {
                    progress.fail();
                    return Err(error);
                }
            }
        }
    }
    progress.finish();
    // Final count — its delivery (then the report stream's FIN) tells the sender
    // the whole stream arrived.
    report.write_all(&done.to_le_bytes()).await?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::{osc, pct};

    #[test]
    fn osc_sequences() {
        assert_eq!(osc(1, 42), "\x1b]9;4;1;42\x1b\\");
        // Indeterminate (3) and clear (0) carry no progress value.
        assert_eq!(osc(3, 0), "\x1b]9;4;3\x1b\\");
        assert_eq!(osc(0, 0), "\x1b]9;4;0\x1b\\");
        assert_eq!(osc(2, 0), "\x1b]9;4;2;0\x1b\\");
    }

    #[test]
    fn percent_clamps_and_rounds() {
        assert_eq!(pct(0, 100), 0);
        assert_eq!(pct(50, 100), 50);
        assert_eq!(pct(100, 100), 100);
        assert_eq!(pct(1, 3), 33);
        assert_eq!(pct(200, 100), 100); // over-report clamps
    }
}
