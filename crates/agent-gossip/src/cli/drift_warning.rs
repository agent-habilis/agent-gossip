/// The stale-skill nag, or `None` when every installed integration matches
/// this binary.
pub(super) fn drift_warning() -> Option<String> {
    super::agent::home_dir()
        .ok()
        .and_then(|home| super::agent::drift_warning(&home))
}
