use habilis_network::protocol::TransportPolicy;
use rmcp::schemars;
use serde::Deserialize;

/// A path a mesh's members may carry payload over — the `--transport` list.
/// `create`-only; every joiner inherits it from the mesh id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Transport {
    /// QUIC on a direct or hole-punched UDP socket.
    #[value(name = "udp")]
    Udp,
    /// QUIC over a `WebRTC` data channel.
    #[value(name = "webrtc")]
    WebRtc,
    /// Reach a peer with no direct path through other members. Needs `udp` or
    /// `webrtc` next to it.
    #[value(name = "multihop")]
    Multihop,
    /// Reach a peer with no other path through the frames of the gossip
    /// itself. Needs `udp` or `webrtc` next to it.
    #[value(name = "gossip")]
    Gossip,
    /// Let payload also fall back to the relay when no direct path exists.
    #[value(name = "relay")]
    Relay,
}

impl From<Transport> for habilis_network::protocol::Transport {
    fn from(transport: Transport) -> Self {
        match transport {
            Transport::Udp => Self::Udp,
            Transport::WebRtc => Self::WebRtc,
            Transport::Multihop => Self::Multihop,
            Transport::Gossip => Self::Gossip,
            Transport::Relay => Self::Relay,
        }
    }
}

/// Resolve a `--transport` list into a [`TransportPolicy`]. The list is
/// literal: it names exactly the paths the mesh carries. `create`-only. Shared
/// by the CLI and the MCP `create_gossip` tool.
///
/// # Errors
/// The list is empty (the engine would give its own default, with multihop
/// on; an absent flag takes the app default instead), names neither `udp` nor
/// `webrtc`, or names `multihop` with no direct path next to it.
pub(crate) fn transport_policy(transports: &[Transport]) -> anyhow::Result<TransportPolicy> {
    anyhow::ensure!(!transports.is_empty(), "the transport list is empty");
    let named: Vec<habilis_network::protocol::Transport> =
        transports.iter().copied().map(Into::into).collect();
    TransportPolicy::from_transports(&named)
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use habilis_network::protocol::TransportPolicy;

    use super::{Transport, transport_policy};
    use crate::cli::args::Cli;

    #[test]
    fn transport_list_is_literal() {
        assert_eq!(
            transport_policy(&[Transport::Udp, Transport::WebRtc]).unwrap(),
            TransportPolicy {
                udp: true,
                webrtc: true,
                multihop: false,
                gossip: false,
                relay_transport: false
            }
        );
    }

    #[test]
    fn transport_names_every_path() {
        let policy = transport_policy(&[
            Transport::Udp,
            Transport::WebRtc,
            Transport::Multihop,
            Transport::Gossip,
            Transport::Relay,
        ])
        .unwrap();
        assert!(policy.multihop && policy.gossip && policy.relay_transport);
    }

    #[test]
    fn transport_empty_list_errors() {
        assert!(transport_policy(&[]).is_err());
    }

    #[test]
    fn transport_relay_alone_errors() {
        assert!(transport_policy(&[Transport::Relay]).is_err());
    }

    #[test]
    fn transport_rejects_unknown_value() {
        assert!(Cli::try_parse_from(["agent-gossip", "create", "--transport", "bogus"]).is_err());
    }
}
