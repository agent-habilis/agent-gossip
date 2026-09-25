//! The crate's public library API — the third frontend beside the `cli` and
//! `mcp` bindings, and the one a Rust program embeds a mesh through.
//!
//! [`MeshSession`] runs the mesh event loop as a background `tokio` task
//! **inside the caller's process** — no subprocess, no Unix-socket IPC. Inbound
//! traffic is pushed over a bounded broadcast channel; outbound sends go through
//! a dedicated channel into the same shared broadcast path the CLI/IPC uses. No
//! `iroh` type crosses this boundary: a join target is a mesh id parsed from a
//! string.

pub(crate) use self::advertise::{DIRECTORY_ADVERTISER_COHOST, spawn_advertiser};
pub use self::config::{CreateConfig, JoinConfig, TopicConfig};
pub use self::directory::{Directory, DirectoryEvent, MeshListing};
pub use self::error::{CreateError, JoinError};
pub(crate) use self::inproc::InProcessSession;
pub use self::params::{A2aCallParams, TaskArtifactParams};
pub use self::session::MeshSession;

mod advertise;
mod config;
mod directory;
pub(crate) use self::directory::resolve_lookups_or_public;
mod error;
mod inproc;
mod params;
mod session;
mod setup;

// Last, so it does not trip `clippy::items_after_test_module`.
#[cfg(test)]
mod tests;
