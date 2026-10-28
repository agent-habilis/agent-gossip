use crate::app_log_pins::APP_LOG_PINS;

/// The default tracing directive filter; pass to
/// `tracing_subscriber::fmt().with_env_filter(..)`. `RUST_LOG` overrides
/// it. See `logging`.
#[must_use]
pub fn log_filter() -> tracing_subscriber::EnvFilter {
    habilis_network::util::logging::log_filter(APP_LOG_PINS)
}
