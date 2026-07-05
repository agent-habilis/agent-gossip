//! Bounded line reads — cap a single `read_line` so no input can grow an
//! unbounded buffer (the "all I/O is bounded" invariant). Used for stdin
//! and the IPC socket.

use tokio::io::{self, AsyncBufRead, AsyncBufReadExt};

/// Outcome of [`read_bounded_line`].
pub(crate) enum LineRead {
    /// Stream closed with nothing buffered.
    Eof,
    /// A complete line (trailing `\n` excluded), within the cap.
    Line(String),
    /// The line exceeded `max` bytes. It was drained through its newline
    /// so the stream stays aligned to the next line, then dropped.
    TooLong,
}

/// Read one `\n`-terminated line, accumulating at most `max` bytes. Uses
/// `fill_buf`/`consume`, so peak memory is `max` + one buffered chunk:
/// once `max` is reached before the newline, further bytes are consumed
/// and discarded until the newline, then `TooLong` is returned.
pub(crate) async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<LineRead> {
    let mut buf: Vec<u8> = Vec::new();
    let mut overflow = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            // EOF.
            if overflow {
                return Ok(LineRead::TooLong);
            }
            if buf.is_empty() {
                return Ok(LineRead::Eof);
            }
            return Ok(finish(&buf));
        }
        if let Some(pos) = chunk.iter().position(|&byte| byte == b'\n') {
            if !overflow {
                let room = max.saturating_sub(buf.len());
                if pos > room {
                    overflow = true;
                } else {
                    buf.extend_from_slice(&chunk[..pos]);
                }
            }
            reader.consume(pos + 1);
            return Ok(if overflow {
                LineRead::TooLong
            } else {
                finish(&buf)
            });
        }
        // No newline in this chunk: accumulate up to the cap, consume all.
        let len = chunk.len();
        if !overflow {
            let room = max.saturating_sub(buf.len());
            if len > room {
                overflow = true;
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        reader.consume(len);
    }
}

fn finish(buf: &[u8]) -> LineRead {
    // Callers `.trim()`; lossy keeps us robust to non-UTF-8 input.
    LineRead::Line(String::from_utf8_lossy(buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{LineRead, io, read_bounded_line};

    #[tokio::test]
    async fn reads_lines_then_eof() {
        let data = b"hello\nworld\n";
        let mut reader = io::BufReader::new(&data[..]);
        assert!(matches!(
            read_bounded_line(&mut reader, 100).await.unwrap(),
            LineRead::Line(line) if line == "hello"
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 100).await.unwrap(),
            LineRead::Line(line) if line == "world"
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 100).await.unwrap(),
            LineRead::Eof
        ));
    }

    #[tokio::test]
    async fn over_long_line_dropped_then_stream_realigns() {
        let data = b"thisaverylongline\nok\n";
        let mut reader = io::BufReader::new(&data[..]);
        assert!(matches!(
            read_bounded_line(&mut reader, 4).await.unwrap(),
            LineRead::TooLong
        ));
        assert!(
            matches!(read_bounded_line(&mut reader, 100).await.unwrap(), LineRead::Line(line) if line == "ok"),
            "the next line must be read intact after dropping the oversize one"
        );
    }

    #[tokio::test]
    async fn final_line_without_newline_is_delivered() {
        let data = b"tail";
        let mut reader = io::BufReader::new(&data[..]);
        assert!(matches!(
            read_bounded_line(&mut reader, 100).await.unwrap(),
            LineRead::Line(line) if line == "tail"
        ));
    }
}
