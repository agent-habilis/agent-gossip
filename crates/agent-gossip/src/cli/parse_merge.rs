use anyhow::Result;

pub(super) fn parse_merge(raw: &str) -> Result<serde_json::Value> {
    serde_json::from_str(raw).map_err(|error| {
        anyhow::anyhow!("--merge must be valid JSON (an RFC 7386 merge patch): {error}")
    })
}
