use super::target::Target;

pub(crate) struct Discovery {
    pub(super) live: Vec<Target>,
    /// State files whose recorded pid is gone (or reused by a non-`agent-gossip`
    /// process) — leftovers of a SIGKILL/power-loss that the daemon never
    /// got to remove itself. Deleted during discovery so they stop
    /// rendering as ghost sessions (e.g. a statusline pill).
    pub(super) cleaned: usize,
}
