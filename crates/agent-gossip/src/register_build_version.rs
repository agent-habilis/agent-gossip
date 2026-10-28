use habilis_network::util::version as engine_version;

use crate::version::version;

/// Hand [`version`] to the engine so its daemon-start log line reports the
/// app's number instead of the engine's own. Idempotent; call before any
/// daemon spawns.
pub(crate) fn register_build_version() {
    engine_version::set_build_version(version());
}
