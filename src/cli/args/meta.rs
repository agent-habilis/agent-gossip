//! `meta` command args: read or change the swarm's `meta` channel — a second
//! shared-state document, identical machinery to `state` but conventionally used
//! for swarm metadata (peer info, …) rather than the task. `meta patch` applies
//! an RFC 6902 patch; `meta get` reads the current document.

use clap::{Parser, Subcommand};

use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct MetaOpts {
    #[command(subcommand)]
    pub action: MetaAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MetaAction {
    /// Apply a JSON-Patch (RFC 6902) change to the `meta` channel.
    ///
    /// Same frozen subset as `state patch`: add/replace/remove on object paths +
    /// add `"/arr/-"`; no test/move/copy, array indices, or root path. The patch
    /// is validated against the current `meta` document and rejected if it does
    /// not apply. The `meta` and `state` channels are fully independent.
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

        /// Compare-and-set guard: the `doc_hash` from your last `meta get`. The
        /// patch is rejected ("stale document", non-zero exit) if the meta
        /// document changed since — re-read and retry.
        #[arg(long = "if-doc-hash")]
        if_doc_hash: Option<String>,
    },

    /// Read the current derived `meta`-channel document.
    Get {
        /// Swarm identifier (🐝...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
    },
}
