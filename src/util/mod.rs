//! Small cross-cutting helpers shared across layers.

pub(crate) mod bounded_fifo_set;
pub(crate) mod bounded_queue;
pub(crate) mod bounded_read;
pub(crate) mod clock;
pub(crate) mod consts;
pub(crate) mod cooldown;
pub(crate) mod logs;
#[expect(
    dead_code,
    reason = "shared cargo-style helpers; the binary uses a subset (e.g. `error` is unused here)"
)]
pub(crate) mod output;
pub(crate) mod process;
pub(crate) mod resident_memory;
pub(crate) mod tuning;
pub(crate) mod version;

/// The per-swarm folder name — the first 16 characters of the swarm
/// identifier. The stem of every per-swarm file (socket / log / state), so it
/// lives here rather than in any one module. See [`swarm_runtime_dir`].
#[must_use]
pub fn swarm_prefix(swarm_id: &str) -> String {
    swarm_id.chars().take(16).collect()
}

/// A swarm's runtime folder — `<RUNTIME_DIR>/<swarm-prefix>/`. All of one
/// swarm's per-member files (`<nick>.tracing.log`, `<nick>.ipc.sock`,
/// `<nick>.state.json`) live here, so the socket, log, and state-file path
/// builders all derive from it.
#[must_use]
pub(crate) fn swarm_runtime_dir(swarm_id: &str) -> std::path::PathBuf {
    std::path::Path::new(consts::RUNTIME_DIR).join(swarm_prefix(swarm_id))
}

#[cfg(test)]
mod tests {
    use super::swarm_prefix;

    #[test]
    fn truncates_to_16_chars() {
        assert_eq!(swarm_prefix("🐝abcdefghijkmnpqrs").chars().count(), 16);
    }

    #[test]
    fn short_input_unchanged() {
        assert_eq!(swarm_prefix("🐝abcd"), "🐝abcd");
    }

    #[test]
    fn result_is_a_prefix_of_input() {
        let input = "🐝abcdefghijkmnpqrstuvwx";
        assert!(input.starts_with(&swarm_prefix(input)));
    }
}
