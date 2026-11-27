use clap::Args;
use habilis_network::protocol::MeshName;

use super::lookup::LookupArgs;
use crate::cli::password_flag::{PasswordFlag, password_flag_parser};

#[derive(Args, Debug)]
pub(crate) struct ExposeArgs {
    /// The local A2A origin to bridge, as a plain http URL (e.g.
    /// `http://127.0.0.1:9999`). https and path-prefixed origins are out of
    /// scope — the bridge forwards raw TCP to a plain-http localhost origin.
    #[arg(long)]
    pub to: String,

    /// Which lookup mechanisms the bridge uses (same flags as `create`):
    /// naming any uses only those; naming none is the all-on public
    /// preset.
    #[command(flatten)]
    pub lookups: LookupArgs,

    /// Advertise this bridge's ticket in a directory so a peer can find it
    /// with `agent-gossip a2a discover` — no ticket to copy. Bare `--advertise` ⇒ the
    /// default `global` directory; `--advertise <name>` ⇒ that named
    /// directory (share the name with the peer). The ad carries the full
    /// bearer ticket, so pair it with `--password`.
    #[arg(long, num_args(0..=1), default_missing_value = "global")]
    pub advertise: Option<MeshName>,

    /// Protect the bridge with a password: the ticket alone no longer
    /// redeems — consumers must present the password (so a passworded ticket
    /// is safe to share). Pass it inline as `--password=<pw>`.
    #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0", value_parser = password_flag_parser())]
    pub password: Option<PasswordFlag>,

    /// Force loopback-only lookups so two agents on one machine bridge
    /// hermetically off the ticket's direct address (no mDNS/DHT/relay).
    /// Hidden — a testing knob.
    #[arg(long, hide = true, default_value_t = false)]
    pub loopback: bool,
}
