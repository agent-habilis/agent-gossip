//! `state` command args: read or change the gossip's shared state — a JSON
//! document every member derives from a dedicated, gossiped log of RFC 7386 JSON
//! Merge Patch changes. `state merge` applies a merge; `state get` reads the
//! current document.

use clap::{Parser, Subcommand};

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct StateOpts {
    #[command(subcommand)]
    pub action: StateAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum StateAction {
    /// Apply an RFC 7386 JSON Merge Patch to the shared state.
    ///
    /// `--merge` is any JSON value, e.g. `'{"turn":"b"}'`. An object
    /// deep-merges (each key is set; a `null` value deletes that key; nested
    /// objects merge recursively; arrays are replaced wholesale — model a
    /// mutable collection as an object keyed by index), and a non-object value
    /// replaces the document.
    Merge {
        /// Gossip identifier (💬...)
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The JSON Merge Patch (any JSON value).
        #[arg(long)]
        merge: String,
    },

    /// Read the current derived shared-state document.
    Get {
        /// Gossip identifier (💬...)
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
    },
}
