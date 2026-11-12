use clap::Subcommand;
use habilis_network::protocol::{MeshId, Nickname};

#[derive(Subcommand, Debug)]
pub(crate) enum StateAction {
    /// Apply an RFC 7386 JSON Merge Patch to the shared state.
    ///
    /// `--merge` is a JSON object, e.g. `'{"turn":"b"}'`. The top-level merge
    /// must be an object: a scalar, an array or `null` at the top level is
    /// refused. An object deep-merges (each key is set; a `null` value deletes
    /// that key; nested objects merge recursively; arrays are replaced
    /// wholesale — model a mutable collection as an object keyed by index),
    /// and a non-object value under a key replaces that key.
    Merge {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The JSON Merge Patch: a JSON object.
        #[arg(long)]
        merge: String,
    },

    /// Read the current derived shared-state document.
    Get {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
    },
}
