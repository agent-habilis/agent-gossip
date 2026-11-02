use habilis_network::util::logging;
use habilis_network::util::logging::LogSink;

/// Build the deferred log sink and register it process-globally.
///
/// Call once in `main` before subscriber init; pass the returned
/// value to `tracing_subscriber::fmt().with_writer(..)`. Logs buffer
/// until `cli` resolves the mesh id + nickname (see `logging`).
#[must_use]
pub fn install_log_sink() -> LogSink {
    logging::install()
}
