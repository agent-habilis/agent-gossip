use anyhow::Result;

/// Parse a `--ttl` value into seconds: a bare number (seconds), a `s`/`m`/`h`/`d`
/// suffixed duration, or `none`/`0` for no expiry. Absent ⇒ the 24h default.
pub(super) fn parse_ttl(raw: Option<&str>) -> Result<u64> {
    let Some(raw) = raw.map(str::trim) else {
        return Ok(habilis_network::util::consts::INVITE_DEFAULT_TTL.as_secs());
    };
    if raw.eq_ignore_ascii_case("none") {
        return Ok(0);
    }
    let (digits, multiplier) = match raw.as_bytes().last() {
        Some(b's' | b'S') => (&raw[..raw.len() - 1], 1),
        Some(b'm' | b'M') => (&raw[..raw.len() - 1], 60),
        Some(b'h' | b'H') => (&raw[..raw.len() - 1], 3600),
        Some(b'd' | b'D') => (&raw[..raw.len() - 1], 86_400),
        _ => (raw, 1),
    };
    let value: u64 = digits.trim().parse().map_err(|_| {
        anyhow::anyhow!("invalid --ttl `{raw}` — use seconds, `1h`, `30m`, `7d`, or `none`")
    })?;
    Ok(value.saturating_mul(multiplier))
}
