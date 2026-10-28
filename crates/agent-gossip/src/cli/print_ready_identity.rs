use super::drift_warning::drift_warning;
use super::ready::ReadyStateFile;

/// Print the session identity as a JSON object for `agent-gossip ready --output json`,
/// omitting any field the state file lacks — so a degenerate (identity-less)
/// file yields `{}` rather than `{"gossip":null,…}` that a caller might splice
/// into the next command as the literal string "null".
pub(super) fn print_ready_identity(ready: &ReadyStateFile) {
    let identity = ready.identity();
    let mut obj = serde_json::Map::new();
    for (key, value) in [
        ("gossip", identity.mesh),
        ("name", identity.name),
        ("nickname", identity.nickname),
        ("topic", identity.topic),
        // The skills tell an agent to print `drift` from *this* output. The
        // daemon computes it too, but only onto its own `--output json` stdout
        // — the stream the session-start script sends to /dev/null — so the
        // warning had no way to reach anyone. It is a local check on the
        // installed skill files, needing no daemon, so `ready` runs it itself.
        ("drift", drift_warning()),
    ] {
        if let Some(value) = value {
            obj.insert(key.to_owned(), value.into());
        }
    }
    println!("{}", serde_json::Value::Object(obj));
}
