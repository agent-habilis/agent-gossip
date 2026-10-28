/// Flush buffered logs to stderr if identity was never resolved
/// (transient command, or startup failed before attach). Call after
/// `run_cli` returns.
pub fn flush_log_if_pending() {
    habilis_network::util::logging::flush_pending_to_stderr();
}
