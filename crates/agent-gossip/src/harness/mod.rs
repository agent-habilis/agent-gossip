//! Out-of-crate test and bench access shims.
//!
//! Each submodule is feature-gated: it widens `pub(crate)` internals so the crate's
//! own bench (`benches/hot_paths.rs`) and adversarial (`tests/adversarial.rs`)
//! suites can reach them, without enlarging the curated public surface.

#[cfg(feature = "bench")]
pub mod bench_config;

#[cfg(feature = "adversarial")]
pub mod test_key;

#[cfg(feature = "adversarial")]
pub mod session;
