//! Shared harness for the `agent-gossip` integration suites.
//!
//! This is a library crate, not a `tests/common/` module, for one reason: each
//! integration binary is its own compilation unit, so a shared `mod common;`
//! makes every helper it does not personally call look dead. Sixteen binaries
//! with wildly different needs (`man_contract` uses one helper; `gossip_network`
//! uses two dozen) made that unavoidable, and the blanket `#![allow(dead_code)]`
//! it forced also hid genuinely dead helpers. A library's `pub` surface is
//! exempt from the lint by construction, so the suppression is gone — while the
//! crate's own private helpers stay `dead_code`-checked.
//!
//! # Example
//!
//! ```rust,no_run
//! use agent_gossip_test_fixtures::{InProcNode, init_test_tracing};
//!
//! init_test_tracing();
//! ```

pub use agent_gossip::{PresenceSubtype, runtime_base};

mod apply_flags;
mod assert_bell_parked;
mod await_mutual_cards;
mod await_peer_card_cli;
mod bell_grace;
mod big_body_timeout;
mod bin;
mod channel_subcommand;
mod chat_message;
mod chat_text;
mod cli_channel_get;
mod cli_channel_merge;
mod cli_message;
mod cli_message_checked;
mod cli_message_raw;
mod cli_msg_checked;
mod cli_msg_raw;
mod cli_peers;
mod cli_ping;
mod cli_poll;
mod cli_poll_long;
mod cli_task_artifact;
mod cli_task_create;
mod cli_task_create_attempt;
mod cli_task_create_raw;
mod cli_task_followup;
mod cli_task_status;
mod connect_timeout;
mod counter;
mod flag_args;
mod followup_params;
mod in_proc_node;
mod init_test_tracing;
mod ipc_raw;
mod is_rpc_timeout;
mod mdns_multicast_available;
mod msg_timeout;
mod node;
mod park_bell;
mod parse_messages;
mod poll;
mod recovery_timeout;
mod rpc_attempt;
mod scan_create_log;
mod serial_gate;
mod serial_guard;
mod socket_path;
mod test_cmd;
mod test_log_dir;
mod test_mesh_name;
mod three_peers;
mod tmp_log;
mod trace_log;
mod wait_bell_output;
mod wait_total;
mod wait_until;

pub(crate) use self::apply_flags::apply_flags;
pub use self::assert_bell_parked::assert_bell_parked;
pub use self::await_mutual_cards::await_mutual_cards;
pub use self::await_peer_card_cli::await_peer_card_cli;
pub use self::bell_grace::BELL_GRACE;
pub use self::big_body_timeout::BIG_BODY_TIMEOUT;
pub use self::bin::bin;
pub use self::channel_subcommand::channel_subcommand;
pub use self::chat_message::ChatMessage;
pub use self::chat_text::chat_text;
pub use self::cli_channel_get::cli_channel_get;
pub use self::cli_channel_merge::cli_channel_merge;
pub use self::cli_message::cli_message;
pub use self::cli_message_checked::cli_message_checked;
pub use self::cli_message_raw::cli_message_raw;
pub use self::cli_msg_checked::cli_msg_checked;
pub use self::cli_msg_raw::cli_msg_raw;
pub use self::cli_peers::cli_peers;
pub use self::cli_ping::cli_ping;
pub use self::cli_poll::cli_poll;
pub use self::cli_poll_long::cli_poll_long;
pub use self::cli_task_artifact::cli_task_artifact;
pub use self::cli_task_create::cli_task_create;
pub(crate) use self::cli_task_create_attempt::cli_task_create_attempt;
pub use self::cli_task_create_raw::cli_task_create_raw;
pub use self::cli_task_followup::cli_task_followup;
pub use self::cli_task_status::cli_task_status;
pub use self::connect_timeout::CONNECT_TIMEOUT;
pub(crate) use self::counter::COUNTER;
pub use self::flag_args::flag_args;
pub use self::followup_params::FollowupParams;
pub use self::in_proc_node::InProcNode;
pub use self::init_test_tracing::init_test_tracing;
pub use self::ipc_raw::ipc_raw;
pub(crate) use self::is_rpc_timeout::is_rpc_timeout;
pub use self::mdns_multicast_available::mdns_multicast_available;
pub use self::msg_timeout::MSG_TIMEOUT;
pub use self::node::Node;
pub use self::park_bell::park_bell;
pub(crate) use self::parse_messages::parse_messages;
pub use self::poll::POLL;
pub use self::recovery_timeout::RECOVERY_TIMEOUT;
pub(crate) use self::rpc_attempt::RPC_ATTEMPT;
pub(crate) use self::scan_create_log::scan_create_log;
pub(crate) use self::serial_gate::SERIAL_GATE;
pub use self::serial_guard::serial_guard;
pub use self::socket_path::socket_path;
pub use self::test_cmd::test_cmd;
pub use self::test_log_dir::test_log_dir;
pub(crate) use self::test_mesh_name::test_mesh_name;
pub use self::three_peers::three_peers;
pub use self::tmp_log::tmp_log;
pub use self::trace_log::trace_log;
pub use self::wait_bell_output::wait_bell_output;
pub use self::wait_total::wait_total;
pub use self::wait_until::wait_until;
