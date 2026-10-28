/// Serializes the daemon-spawning reliability tests (`#[test]`, sync) —
/// beacon migration, sleep/wake heal, anti-entropy, flap storms. They assert
/// recovery within heal-cadence-gated budgets; run concurrently (libtest
/// `--test-threads`), their active windows starve each other's timing and
/// miss those budgets — a flaky failure that is **not** a product bug (real
/// daemons mesh in ~3s; verified out-of-band). Holding this gate for the
/// test's duration lets at most one run at a time while lighter tests still
/// parallelize. Each integration binary is its own process, so this `static`
/// serializes within a binary, not across them. (The async in-process
/// adversarial suite does **not** use this — a multi-thread runtime keeps it
/// responsive; a cross-runtime async mutex starved it instead.)
pub(crate) static SERIAL_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
