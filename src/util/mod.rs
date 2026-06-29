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
    reason = "canonical cargo-style helpers, also include!d by the cargo task runner; the binary uses a subset"
)]
pub(crate) mod output;
pub(crate) mod resident_memory;
pub(crate) mod tuning;
pub(crate) mod version;

/// The `<swarm_prefix>-<nick>` filename stem — the `🐝` brand stripped, then
/// the first 16 Base58 characters of the swarm identifier, so the stem stays
/// ASCII / path-safe (an emoji in a socket/log filename is non-portable).
/// Shared by both the socket name ([`consts::SOCKET_DIR`]) and the log file
/// name ([`logs::log_file_path`]), so it lives here rather than in either module.
#[must_use]
pub fn swarm_prefix(swarm_id: &str) -> String {
    swarm_id
        .strip_prefix('🐝')
        .unwrap_or(swarm_id)
        .chars()
        .take(16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::swarm_prefix;

    #[test]
    fn truncates_to_16_chars() {
        assert_eq!(swarm_prefix("🐝abcdefghijkmnpqrs").chars().count(), 16);
    }

    #[test]
    fn short_input_unchanged_after_strip() {
        assert_eq!(swarm_prefix("🐝abcd"), "abcd");
    }

    #[test]
    fn strips_the_bee_so_paths_stay_ascii() {
        let stem = swarm_prefix("🐝abcdefghijkmnpqrstuvwx");
        assert!(!stem.contains('🐝'), "no emoji in a file-path stem");
        assert!(stem.is_ascii());
        assert!("abcdefghijkmnpqrstuvwx".starts_with(&stem));
    }
}
