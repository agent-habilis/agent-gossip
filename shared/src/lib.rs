//! Constants and helpers shared across the workspace (the `ahs` binary
//! crate and the xtask runner). Kept dependency-free so xtask stays light.
//!
//! - [`consts`] — the single home for tunable constants (runtime paths +
//!   wire contract), re-exported at the crate root for ergonomics.
//! - [`logs`] — per-member log path resolution.

pub mod consts;
pub mod logs;

pub use consts::*;

/// The `<swarm_prefix>-<nick>` filename stem — the first 16 characters
/// of the swarm identifier. Shared by both the socket name
/// ([`crate::consts::SOCKET_DIR`]) and the log file name
/// ([`logs::log_file_path`]), so it lives at the crate root rather than
/// in either module.
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
