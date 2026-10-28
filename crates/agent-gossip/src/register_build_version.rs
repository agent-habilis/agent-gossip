use crate::version::version;

/// Hand [`version`] to the engine so its daemon-start log line reports the
/// app's number instead of the engine's own. Idempotent; call before any
/// daemon spawns.
pub(crate) fn register_build_version() {
    habilis_network::util::version::set_build_version(version());
}
