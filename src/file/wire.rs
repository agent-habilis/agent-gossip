//! Small async read helpers for the framed file protocol. All integers are
//! little-endian, matching the manifest and ticket codecs.

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt};

pub(crate) async fn read_u16<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf).await?;
    Ok(u16::from_le_bytes(buf))
}

pub(crate) async fn read_u32<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    Ok(u32::from_le_bytes(buf))
}

pub(crate) async fn read_u64<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf).await?;
    Ok(u64::from_le_bytes(buf))
}

pub(crate) async fn read_i64<R: AsyncRead + Unpin>(reader: &mut R) -> Result<i64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf).await?;
    Ok(i64::from_le_bytes(buf))
}

/// Read a `u16`-length-prefixed UTF-8 string (a path or the root name).
pub(crate) async fn read_str<R: AsyncRead + Unpin>(reader: &mut R) -> Result<String> {
    let len = usize::from(read_u16(reader).await?);
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    String::from_utf8(buf).context("received a non-UTF-8 string")
}
