//! The `agent-gossip` command-line interface: the clap-derived argument shape
//! lives in [`args`], the `discover` JSON stream in [`discover`], and the
//! per-subcommand handlers + [`dispatch`] here. `lib.rs::run_cli` parses
//! argv and calls `dispatch`; each handler is the thin glue between the
//! parsed args and the daemon / IPC / api layers it drives.

pub(crate) use self::args::Cli;
pub(crate) use self::dispatch::dispatch;

mod a2a;
mod a2a_call_request;
mod a2a_call_sugar;
mod advertised_file_name;
pub(crate) mod agent;
pub(crate) mod args;
pub(crate) mod bell;
mod create;
mod detach_if_owned;
mod detached_stdio;
mod discover;
mod discover_params;
mod discovery;
mod dispatch;
mod doctor;
mod drift_warning;
mod finish_send;
mod install_tuning;
mod invite;
mod join;
mod meta;
mod msg_response;
mod parse_ttl;
mod password_flag;
mod peers;
mod ping;
mod plug;
mod poll;
mod print_ready_identity;
mod ready_state_file;
mod reject_id_encoded_flag;
mod run_session;
mod send;
mod session;
mod signal;
mod state;
mod target;
#[cfg(test)]
mod tests;
mod topic;
mod topology_cmd;
