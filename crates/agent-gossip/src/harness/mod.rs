//! Out-of-crate test/bench access shims. Each submodule is feature-gated
//! and `#[doc(hidden)]`: it widens `pub(crate)` internals so the crate's
//! own bench (`benches/hot_paths.rs`) and adversarial (`tests/adversarial.rs`)
//! suites can reach them, without enlarging the curated public surface.

#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench_config;

#[cfg(feature = "adversarial")]
#[doc(hidden)]
pub mod test_key;

#[cfg(feature = "adversarial")]
#[doc(hidden)]
pub mod session;
