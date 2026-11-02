//! The crate's public library API — the third frontend beside the `cli` and
//! `mcp` bindings, and the one a Rust program embeds a mesh through.
//!
//! [`MeshSession`] runs the mesh event loop as a background `tokio` task
//! **inside the caller's process** — no subprocess, no Unix-socket IPC. Inbound
//! traffic is pushed over a bounded broadcast channel; outbound sends go through
//! a dedicated channel into the same shared broadcast path the CLI/IPC uses. No
//! `iroh` type crosses this boundary: a join target is a mesh id parsed from a
//! string.

pub use self::a2a_call_params::{A2aCallParams, TaskArtifactParams, TaskStatusParams};
pub(crate) use self::advertiser::{
    DIRECTORY_ADVERTISER_COHOST, initial_live_count, spawn_advertiser,
};
pub use self::create_error::{CreateError, JoinError};
pub use self::directory::{Directory, DirectoryEvent, MeshListing};
pub use self::directory_error::DirectoryError;
pub(crate) use self::in_process_session::InProcessSession;
pub use self::join_config::{CreateConfig, JoinConfig, TopicConfig};
pub use self::mesh_session::MeshSession;
pub use self::session_error::SessionError;

mod a2a_call_params;
mod advertiser;
mod create_error;
pub(crate) mod directory;
mod directory_error;
mod in_process_session;
mod join_config;
mod mesh_session;
mod session_error;
mod spawn_env;

// Last, so it does not trip `clippy::items_after_test_module`.
#[cfg(test)]
mod tests;
