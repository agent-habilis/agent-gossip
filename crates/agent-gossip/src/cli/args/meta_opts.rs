//! `meta` command args: read or change the gossip's `meta` channel — a second
//! shared-state document, identical machinery to `state` but conventionally used
//! for gossip metadata (peer info, …) rather than the task. `meta merge` applies
//! an RFC 7386 JSON Merge Patch; `meta get` reads the current document.

use clap::Parser;

use super::meta_action::MetaAction;

#[derive(Parser, Debug)]
pub(crate) struct MetaOpts {
    #[command(subcommand)]
    pub action: MetaAction,
}
