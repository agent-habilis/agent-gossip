// The app's status helpers, shared rather than forked: both audiences want the
// same cargo-style colored lines, and `anstream` already strips the color for
// whichever of them is piping.
pub(crate) use agent_gossip::status as output;

mod ensure_installed;
mod invocation;
mod repo_root;
mod sweep_stale_artifacts;

pub(crate) use self::ensure_installed::ensure_installed;
pub(crate) use self::invocation::Invocation;
pub(crate) use self::repo_root::repo_root;
pub(crate) use self::sweep_stale_artifacts::sweep_stale_artifacts;
