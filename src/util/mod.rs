//! Small cross-cutting helpers shared across layers.

pub(crate) mod bounded_fifo_set;
pub(crate) mod bounded_queue;
pub(crate) mod bounded_read;
pub(crate) mod clock;
pub(crate) mod consts;
pub(crate) mod cooldown;
pub(crate) mod logs;
pub(crate) mod resident_memory;
pub(crate) mod tuning;
pub(crate) mod version;

/// The `<swarm_prefix>-<nick>` filename stem — the first 16 characters
/// of the swarm identifier. Shared by both the socket name
/// ([`consts::SOCKET_DIR`]) and the log file name
/// ([`logs::log_file_path`]), so it lives here rather than in either module.
#[must_use]
pub fn swarm_prefix(swarm_id: &str) -> String {
    swarm_id.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::swarm_prefix;

    #[test]
    fn truncates_to_16_chars() {
        assert_eq!(swarm_prefix("ahsabcdefghijkmnpqrs").chars().count(), 16);
    }

    #[test]
    fn short_input_unchanged() {
        assert_eq!(swarm_prefix("ahsabcd"), "ahsabcd");
    }

    #[test]
    fn result_is_a_prefix_of_input() {
        let input = "ahsabcdefghijkmnpqrstuvwx";
        assert!(input.starts_with(&swarm_prefix(input)));
    }
}
