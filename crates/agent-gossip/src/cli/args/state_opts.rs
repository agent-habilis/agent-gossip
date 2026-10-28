//! `state` command args: read or change the gossip's shared state — a JSON
//! document every member derives from a dedicated, gossiped log of RFC 7386 JSON
//! Merge Patch changes. `state merge` applies a merge; `state get` reads the
//! current document.

use clap::Parser;

use super::state_action::StateAction;

#[derive(Parser, Debug)]
pub(crate) struct StateOpts {
    #[command(subcommand)]
    pub action: StateAction,
}
