use rmcp::schemars;
use serde::Deserialize;

use crate::cli::args::lookup::Lookup;
use crate::cli::args::transport::Transport;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateMeshArgs {
    /// Human-readable gossip name. Optional — omit for a random
    /// `word-word` name (the same style as the nickname). When given:
    /// 1..=32 UTF-8 characters (any script/emoji), excluding control
    /// characters, whitespace, and any of / \ < > #. Bound
    /// cryptographically into the gossip identity so joiners decode the
    /// same name and forgery is infeasible.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Optional nickname in `word-word` form. Random if omitted.
    #[serde(default)]
    pub(crate) nickname: Option<String>,
    /// Networking lookups: any of "mdns", "dht", "relay". Naming any
    /// restricts to those; an empty list makes a loopback-only gossip, or one
    /// with all three when `advertise` is true (the same model as the CLI
    /// `--lookup` flag).
    #[serde(default)]
    pub(crate) lookup: Vec<Lookup>,
    /// Ordered relay ladder. Requires "relay" in `lookup`; omit for the
    /// default relay set.
    #[serde(default)]
    pub(crate) relay_urls: Vec<String>,
    /// Which paths may carry mesh payload: any of "udp", "webrtc", "multihop",
    /// "gossip", "relay". The list is literal and needs "udp" or "webrtc";
    /// "relay" needs "relay" in `lookup`. Omit it for the default, `["udp",
    /// "webrtc","gossip"]` plus "relay" when "relay" is in `lookup`.
    #[serde(default)]
    pub(crate) transport: Vec<Transport>,
    /// List this gossip in a directory so others can find it with
    /// `agent-gossip discover` (no id to share). Requires a lookup that
    /// reaches other machines. Note: advertising broadcasts the join
    /// token — the gossip becomes open to anyone discovering the directory.
    #[serde(default)]
    pub(crate) advertise: bool,
    /// The directory to advertise into when `advertise` is true.
    /// Omit for the well-known `global` directory.
    #[serde(default)]
    pub(crate) directory: Option<String>,
    /// Protect the gossip with a password: joiners must present it, and the
    /// id alone no longer admits (safe to advertise). Never prompts.
    #[serde(default)]
    pub(crate) password: Option<String>,
}
