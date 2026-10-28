//! The command-line surface: `Cli` + the `Commands` enum tie the
//! per-command argument structs together. Each command's args (and its
//! parse tests) live in their own file — `create`, `join`, `msg`,
//! `poll`, `ping`, `discover` — over the shared building blocks
//! `shared` (`SharedServerOpts`) and `lookup` (`LookupArgs`). The
//! imperative per-command logic lives in the parent [`super`] module.

pub(crate) use self::a2a_action::A2aAction;
pub(crate) use self::bell_check_opts::BellCheckOpts;
pub(crate) use self::cli::Cli;
pub(crate) use self::commands::Commands;
pub(crate) use self::create_opts::CreateOpts;
pub(crate) use self::discover_opts::DiscoverOpts;
pub(crate) use self::doctor_opts::DoctorOpts;
pub(crate) use self::invite_opts::InviteOpts;
pub(crate) use self::leave_opts::LeaveOpts;
pub(crate) use self::meta_action::MetaAction;
pub(crate) use self::meta_opts::MetaOpts;
pub(crate) use self::output_format::OutputFormat;
pub(crate) use self::peers_opts::PeersOpts;
pub(crate) use self::ping_opts::PingOpts;
pub(crate) use self::poll_identity::PollIdentity;
pub(crate) use self::poll_opts::PollOpts;
pub(crate) use self::ready_opts::ReadyOpts;
pub(crate) use self::session_opts::SessionOpts;
pub(crate) use self::shared_server_opts::SharedServerOpts;
pub(crate) use self::state_action::StateAction;
pub(crate) use self::state_opts::StateOpts;
pub(crate) use self::topic_opts::TopicOpts;
pub(crate) use self::topology_opts::TopologyOpts;

mod a2a_action;
mod a2a_opts;
mod bell_check_opts;
mod create_opts;
mod discover_opts;
mod doctor_opts;
mod invite_opts;
mod join_opts;
mod leave_opts;
mod legacy_output;
pub(crate) mod lookup;
mod meta_action;
mod meta_opts;
mod output_format;
mod peers_opts;
mod ping_opts;
mod poll_identity;
mod poll_opts;
mod ready_opts;
pub(crate) mod resolved;
mod session_opts;
mod shared_server_opts;
mod state_action;
mod state_opts;
mod topic_opts;
mod topology_opts;
pub(crate) mod transport;
pub(crate) mod tuning_opts;

mod cli;
mod commands;
