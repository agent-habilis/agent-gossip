//! `state` command args: read or change the swarm's shared state — a JSON
//! document every member derives from a dedicated, gossiped log of JSON-Patch
//! changes. `state patch` applies an RFC 6902 patch; `state get` reads the
//! current document.

use clap::{Parser, Subcommand};

use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct StateOpts {
    #[command(subcommand)]
    pub action: StateAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum StateAction {
    /// Apply a JSON-Patch (RFC 6902) change to the shared state.
    ///
    /// `--patch` is the op array, e.g.
    /// `'[{"op":"replace","path":"/turn","value":"b"}]'`. Frozen subset:
    /// add/replace/remove on object paths + add `"/arr/-"`; no
    /// test/move/copy, array indices, or root path. The patch is validated
    /// against the current document and rejected if it does not apply.
    Patch {
        /// Swarm identifier (🐝...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The JSON-Patch op array.
        #[arg(long)]
        patch: String,

        /// Compare-and-set guard: the `doc_hash` from your last `state get`.
        /// The patch is rejected ("stale document", non-zero exit) if the
        /// document changed since — re-read and retry. Use it for turn-based or
        /// contended state so a concurrent peer's change isn't clobbered.
        #[arg(long = "if-doc-hash")]
        if_doc_hash: Option<String>,
    },

    /// Read the current derived shared-state document.
    Get {
        /// Swarm identifier (🐝...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
    },
}
