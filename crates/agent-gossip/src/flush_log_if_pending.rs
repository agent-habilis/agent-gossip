/// Flush buffered logs to stderr if identity was never resolved
/// (transient command, or startup failed before attach). Call after
/// `run_cli` returns.
use habilis_network::util::logging;
pub fn flush_log_if_pending() {
    logging::flush_pending_to_stderr();
}
