//! `meta` command args: read or change the gossip's `meta` channel — a second
//! shared-state document, identical machinery to `state` but conventionally used
//! for gossip metadata (peer info, …) rather than the task. `meta merge` applies
//! an RFC 7386 JSON Merge Patch; `meta get` reads the current document.

use clap::{Parser, Subcommand};

use fofoca::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct MetaOpts {
    #[command(subcommand)]
    pub action: MetaAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MetaAction {
    /// Apply an RFC 7386 JSON Merge Patch to the `meta` channel.
    ///
    /// Same merge semantics as `state merge`: an object deep-merges (a `null`
    /// value deletes that key, nested objects merge) and a non-object value
    /// replaces the document. By convention agents self-report under
    /// `/peers/<nickname>`, e.g.
    /// `'{"peers":{"word-word":{"model":"Opus 4.8","harness":"Claude Code","host":"studio-mbp-01"}}}'`
    /// — merge means your entry never clobbers another peer's. The `meta` and
    /// `state` channels are fully independent.
    Merge {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The JSON Merge Patch (any JSON value).
        #[arg(long)]
        merge: String,
    },

    /// Read the current derived `meta`-channel document.
    Get {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
    },
}
