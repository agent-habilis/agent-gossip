use rmcp::schemars;
use serde::Deserialize;

use fofoca::protocol::{LookupOpts, RelayChoice, TransportPolicy};

/// A payload path a mesh's members may use — the `--transport` policy.
/// `create`-only; every joiner inherits it from the mesh id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Transport {
    /// Direct paths only (hole-punched IP or `WebRTC`). The default.
    #[value(name = "p2p")]
    P2p,
    /// Let payload also fall back to the relay when a direct path fails.
    #[value(name = "relay")]
    Relay,
}

/// Resolve `--transport` into a [`TransportPolicy`]. `create`-only. Shared by
/// the CLI and the MCP `create_gossip` tool.
///
/// # Errors
/// The list is non-empty and does not name `p2p`.
pub(crate) fn transport_policy(transports: &[Transport]) -> anyhow::Result<TransportPolicy> {
    if transports.is_empty() {
        return Ok(TransportPolicy::default());
    }
    if !transports.contains(&Transport::P2p) {
        anyhow::bail!("p2p cannot be disabled: use `p2p` or `p2p,relay`");
    }
    Ok(TransportPolicy {
        relay_transport: transports.contains(&Transport::Relay),
    })
}

/// The one cross-field rule between `--transport` and `--lookup`: letting the
/// relay carry payload needs a relay to exist as a lookup. The engine checks
/// the same rule in `MeshConfig::validate`, but names its own field there —
/// this check runs first, naming the flags.
///
/// # Errors
/// `policy.relay_transport` is set but `lookups.relay_lookup` is `Disabled`.
pub(crate) fn check_relay_transport(
    policy: TransportPolicy,
    lookups: &LookupOpts,
) -> anyhow::Result<()> {
    if policy.relay_transport && lookups.relay_lookup == RelayChoice::Disabled {
        anyhow::bail!("--transport p2p,relay needs a relay lookup: add `relay` to --lookup");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Transport, check_relay_transport, transport_policy};
    use crate::cli::args::Cli;
    use fofoca::protocol::{LookupOpts, TransportPolicy};

    #[test]
    fn transport_absent_and_p2p_are_lookup_only() {
        assert_eq!(transport_policy(&[]).unwrap(), TransportPolicy::default());
        assert_eq!(
            transport_policy(&[Transport::P2p]).unwrap(),
            TransportPolicy {
                relay_transport: false
            }
        );
    }

    #[test]
    fn transport_p2p_relay_sets_relay_transport() {
        let policy = transport_policy(&[Transport::P2p, Transport::Relay]).unwrap();
        assert!(policy.relay_transport);
    }

    #[test]
    fn transport_relay_alone_errors() {
        assert!(transport_policy(&[Transport::Relay]).is_err());
    }

    #[test]
    fn transport_rejects_unknown_value() {
        assert!(Cli::try_parse_from(["agent-gossip", "create", "--transport", "webrtc"]).is_err());
    }

    #[test]
    fn relay_transport_without_relay_lookup_errors() {
        let policy = TransportPolicy {
            relay_transport: true,
        };
        let opts = LookupOpts::loopback();
        assert!(check_relay_transport(policy, &opts).is_err());
    }
}
