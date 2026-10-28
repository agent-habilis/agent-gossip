//! `a2a` command args. Two families under one subcommand:
//! - **native A2A messaging** (`a2a call` / `status` / `artifact`) — the
//!   gossip request/response + worker-push surface for gossip tasks; and
//! - **the A2A HTTP tunnel** (`a2a expose` / `connect` / `discover`) — bridge a
//!   local A2A HTTP server to a peer over the gossip (a ticket, 1:1).

use clap::Parser;

use super::a2a_action::A2aAction;

#[derive(Parser, Debug)]
pub(crate) struct A2aOpts {
    #[command(subcommand)]
    pub action: A2aAction,
}
