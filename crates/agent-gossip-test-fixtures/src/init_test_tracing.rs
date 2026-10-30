/// Route the daemon's `tracing` to the test's captured stdout when `RUST_LOG` is set.
///
/// This lets an in-process failure can be debugged the same way a subprocess
/// one can (`RUST_LOG=habilis_network::gossip=debug cargo test …`). Without this
/// the in-process tests install no subscriber at all and every diagnostic the
/// engine emits is discarded — the reason a flake here had to be chased by
/// bisecting asserts rather than reading a log.
///
/// Idempotent and silent when `RUST_LOG` is unset: `try_init` fails harmlessly
/// if a subscriber is already installed, and an empty filter emits nothing, so
/// a normal run is unaffected.
pub fn init_test_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}
