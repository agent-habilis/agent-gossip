//! Standing a node up and driving it: resolve inputs, build the config, run.
//!
//! The whole lifecycle is three calls — `Params::resolve()` →
//! [`setup_mesh`] → [`Node::spawn`] (or [`run`] for a consumer that owns the
//! task itself). [`EventLoopConfig`] is opaque between the second and third.

pub use crate::daemon::config::{
    CoHostPolicy, DIRECTORY_ADVERTISER_COHOST, DriverMode, EventLoopConfig,
};
pub use crate::daemon::event_loop::run;
pub use crate::daemon::node::Node;
pub use crate::daemon::params::{
    CreateParams, JoinParams, Resolved, TopicParams, derive_topic_mesh,
};
pub use crate::daemon::setup::{SetupKind, SetupParams, setup_mesh};

/// The session state file the daemon writes for external readers — its sole
/// writer. Daemon-session state, not a generic filesystem helper.
pub mod state_file {
    pub use crate::daemon::state_file::{
        ReadySnapshot, SessionEntry, SessionIdentity, StateFile, read_identity, read_session_entry,
        read_snapshot,
    };
}

/// The local control socket a CLI-driven daemon serves, and its client half.
pub mod ipc {
    pub use crate::transport::ipc::{
        Addressed, NoDaemon, active_socket_paths, json_ack, json_error, json_ok_msg, send,
        send_to_path,
    };
}

/// Per-process dials, installed once at startup. No environment variables:
/// every knob is a `const` default a consumer may override through this.
pub mod tuning {
    pub use crate::util::tuning::{
        NODE_INBOUND_CAP, POLL_LONG_MIN_CYCLE_MS, READY_FRESH_SECS, READY_MAX_SECS,
        READY_POLL_INTERVAL_MS, Tuning, advertise_interval_secs, directory_expiry_secs,
        directory_private_for_test, init, ping_window_secs,
    };
}
