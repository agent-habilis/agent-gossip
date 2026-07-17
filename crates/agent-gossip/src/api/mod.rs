//! The crate's public library API — the third frontend beside the `cli` and
//! `mcp` bindings, and the one a Rust program embeds a mesh through.
//!
//! [`MeshSession`] runs the mesh event loop as a background `tokio` task
//! **inside the caller's process** — no subprocess, no Unix-socket IPC. Inbound
//! traffic is pushed over a bounded broadcast channel; outbound sends go through
//! a dedicated channel into the same shared broadcast path the CLI/IPC uses. No
//! `iroh` type crosses this boundary: a join target is a `💬…` id parsed from a
//! string.

mod advertise;
mod config;
mod directory;
mod error;
mod inproc;
mod params;
mod session;
mod setup;

pub use config::{CreateConfig, JoinConfig, TopicConfig};
pub use directory::{Directory, DirectoryEvent, MeshListing};
pub use error::{CreateError, JoinError};
pub use params::{A2aCallParams, TaskArtifactParams};
pub use session::MeshSession;

pub(crate) use advertise::{DIRECTORY_ADVERTISER_COHOST, spawn_advertiser};
pub(crate) use inproc::InProcessSession;

// Last, so it does not trip `clippy::items_after_test_module`.
#[cfg(test)]
mod tests;
