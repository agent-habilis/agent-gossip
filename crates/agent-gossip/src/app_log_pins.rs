/// This crate's own `tracing` targets, pinned to `info`.
///
/// **Load-bearing.** A release build's base level is `error`, so a target that
/// matches no pin compiles, works in a debug build, and is silently dropped from
/// every optimized one — the exact failure that once made a reliability test go
/// red only under `--profile ci`. Any new `target:` in this crate belongs here.
///
/// The engine cannot hold these: it lives in its own repo with other consumers
/// and has no business naming ours — the pins for a consumer's targets are the
/// consumer's business.
///
/// `pub` + `doc(hidden)` so `tests/log_pins.rs` can assert this list covers
/// every `target:` the crate actually emits under. Not public API.
#[doc(hidden)]
pub const APP_LOG_PINS: &str = "agent_gossip::a2a=info,agent_gossip::directory=info";
