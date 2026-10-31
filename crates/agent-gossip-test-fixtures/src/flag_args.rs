/// Turn `(flag, value)` tuning pairs into CLI args for a spawned `agent-gossip`.
///
/// This replaces the former `.envs(...)` overrides. An empty value yields a
/// bare flag — e.g. the boolean `("--directory-private", "")`. For pair lists
/// that never include `RUST_LOG` (directory / monitor spawns); use
/// `apply_flags` otherwise.
pub fn flag_args(pairs: &[(&str, &str)]) -> impl Iterator<Item = String> {
    pairs.iter().flat_map(|(flag, value)| {
        let mut out = vec![(*flag).to_string()];
        if !value.is_empty() {
            out.push((*value).to_string());
        }
        out
    })
}
