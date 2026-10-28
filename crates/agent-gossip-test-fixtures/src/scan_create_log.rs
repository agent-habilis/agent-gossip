/// Scan a `create`/`join` log's full contents so far for the mesh id and
/// nickname, filling in whichever of `mesh_id`/`nickname` is still unset. The
/// daemon's JSON `ready` event carries both (`gossip` / `nickname`).
pub(crate) fn scan_create_log(
    content: &str,
    mesh_id: &mut Option<String>,
    nickname: &mut Option<String>,
) {
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value["event"] != "ready" {
            continue;
        }
        if mesh_id.is_none() {
            *mesh_id = value["gossip"].as_str().map(str::to_owned);
        }
        if nickname.is_none() {
            *nickname = value["nickname"].as_str().map(str::to_owned);
        }
    }
}
