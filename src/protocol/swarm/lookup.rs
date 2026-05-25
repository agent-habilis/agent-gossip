//! The lookup allowlist (address-lookup + relay) and the `--advertise`
//! directory selection — the flag-shaped inputs the CLI resolves into the
//! effective [`LookupOpts`] the endpoint builder applies.

use anyhow::{Result, bail};
use iroh::RelayUrl;

use super::{SwarmMode, SwarmName};

/// The connectivity relay resolved from the allowlist. `Disabled` ⇒
/// no relay at all (`RelayMode::Disabled`); `Pinned` ⇒ the
/// lookup-layer pinned default *ladder* (the n0 prod set); `Custom` ⇒
/// an operator-supplied **ordered ladder** (`--relay a,b,c`). Relay is
/// an allowlist member like mdns/dht, not an always-on URL — the lookup
/// layer turns `Pinned`/`Custom` into an ordered relay ladder, and the
/// beacon homes on the first reachable rung (see `lookup::relay_ladder`
/// / `lookup::select_bootstrap_rung`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayChoice {
    Disabled,
    Pinned,
    Custom(Vec<RelayUrl>),
}

/// The resolved lookup + connectivity config the endpoint builder
/// applies. `mdns`/`dht` are the enabled iroh address-lookups (both
/// resolve the same seed-derived `rendezvous_id`); `relay` is the
/// connectivity relay (see [`RelayChoice`]).
#[derive(Debug, Clone)]
pub(crate) struct LookupOpts {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelayChoice,
}

impl LookupOpts {
    /// The default behaviour, kept stable for the in-process
    /// embed/MCP sessions: `private` ⇒ everything off (loopback
    /// ladder); `public` ⇒ all lookups (mdns + dht) + the pinned (or
    /// custom) relay. Built directly rather than via the allowlist:
    /// a custom relay ladder must *not* suppress mdns/dht here (the
    /// embed/MCP contract is all-lookups-on), unlike the CLI allowlist
    /// where naming `--relay` alone restricts to relay only. An empty
    /// `relay` ladder ⇒ the pinned default.
    pub(crate) fn default_for(mode: SwarmMode, relay: Vec<RelayUrl>) -> Self {
        match mode {
            // `relay` is ignored on private: upstream `resolve_relay`
            // already rejects a private relay before we get here.
            SwarmMode::Private => LookupOpts {
                mdns: false,
                dht: false,
                relay: RelayChoice::Disabled,
            },
            SwarmMode::Public => LookupOpts {
                mdns: true,
                dht: true,
                relay: if relay.is_empty() {
                    RelayChoice::Pinned
                } else {
                    RelayChoice::Custom(relay)
                },
            },
        }
    }
}

/// CLI `--relay` intent: absent / bare / valued. Resolved into a
/// [`RelayChoice`] by [`resolve_lookups`]. `Custom` carries the ordered
/// ladder parsed from a comma-separated `--relay a,b,c`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum RelaySelection {
    #[default]
    Unset,
    Default,
    Custom(Vec<RelayUrl>),
}

impl RelaySelection {
    fn is_set(&self) -> bool {
        !matches!(self, RelaySelection::Unset)
    }
}

/// CLI `--advertise` intent: absent / bare / valued — the same
/// three-state optional-value shape as [`RelaySelection`]. `Unset` ⇒
/// the swarm is not listed in any directory; `Default` ⇒ the well-known
/// `global` directory; `Named` ⇒ a custom directory. The directory name is itself a
/// [`SwarmName`] (same charset), since the directory derives its
/// swarm from it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum DirectorySelection {
    #[default]
    Unset,
    Default,
    Named(SwarmName),
}

/// The well-known default directory — used when `--advertise` is passed
/// bare (no value).
pub(crate) const DEFAULT_DIRECTORY: &str = "global";

impl DirectorySelection {
    /// `true` when advertising is requested at all (bare or valued).
    pub(crate) fn is_set(&self) -> bool {
        !matches!(self, DirectorySelection::Unset)
    }

    /// The directory to advertise into, or `None` when not advertising.
    /// Bare ⇒ the [`DEFAULT_DIRECTORY`]; valued ⇒ the given name.
    pub(crate) fn directory(&self) -> Option<SwarmName> {
        match self {
            DirectorySelection::Unset => None,
            DirectorySelection::Default => Some(
                SwarmName::new(DEFAULT_DIRECTORY).expect("DEFAULT_DIRECTORY is a valid swarm name"),
            ),
            DirectorySelection::Named(name) => Some(name.clone()),
        }
    }
}

/// `--advertise` lists the swarm in a public directory, so it requires
/// the public network — mirrors [`validate_lookups`]. A bare/valued
/// `--advertise` with a private (loopback-only) swarm is a hard error,
/// never a silent no-op.
pub(crate) fn validate_advertise(mode: SwarmMode, advertise: &DirectorySelection) -> Result<()> {
    if advertise.is_set()
        && mode != SwarmMode::Public
        && !crate::util::tuning::directory_private_for_test()
    {
        bail!("--advertise requires the public network; pass --public");
    }
    Ok(())
}

/// The selected lookup allowlist: the mechanisms a member can
/// enable. `mdns`/`dht` are address-lookups; `relay` is the
/// connectivity/relay-direct rendezvous path — all three obey the same
/// allowlist rule.
#[derive(Debug, Clone, Default)]
pub(crate) struct LookupSet {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelaySelection,
}

impl LookupSet {
    fn any(&self) -> bool {
        self.mdns || self.dht || self.relay.is_set()
    }
}

/// One network-compatibility guard (generalises `require_relay_public`
/// to every lookup flag). `private` is loopback-only, so any
/// `--mdns`/`--dht`/`--relay` is rejected, naming them all in a single
/// message — never a silent no-op.
pub(crate) fn validate_lookups(mode: SwarmMode, lookups: &LookupSet) -> Result<()> {
    if mode == SwarmMode::Public {
        return Ok(());
    }
    let mut offending = Vec::new();
    if lookups.mdns {
        offending.push("--mdns");
    }
    if lookups.dht {
        offending.push("--dht");
    }
    if lookups.relay.is_set() {
        offending.push("--relay");
    }
    if offending.is_empty() {
        Ok(())
    } else {
        bail!(
            "{} require the public network; pass --public",
            offending.join(", ")
        );
    }
}

/// Resolve the allowlist against the network mode into the effective
/// [`LookupOpts`]. On `public`: naming **no** flag enables all three
/// (mdns + dht + pinned relay); naming **any** uses *only* those passed
/// — so `--mdns` alone disables both dht and the relay. `--relay` bare
/// ⇒ pinned default, `--relay <url>` ⇒ custom. Errors if any
/// `--mdns`/`--dht`/`--relay` is given with `private`.
pub(crate) fn resolve_lookups(mode: SwarmMode, lookups: LookupSet) -> Result<LookupOpts> {
    validate_lookups(mode, &lookups)?;
    match mode {
        SwarmMode::Private => Ok(LookupOpts {
            mdns: false,
            dht: false,
            relay: RelayChoice::Disabled,
        }),
        SwarmMode::Public if lookups.any() => {
            // Any flag ⇒ use *only* those passed.
            let relay = match lookups.relay {
                RelaySelection::Unset => RelayChoice::Disabled,
                RelaySelection::Default => RelayChoice::Pinned,
                RelaySelection::Custom(ladder) => RelayChoice::Custom(ladder),
            };
            Ok(LookupOpts {
                mdns: lookups.mdns,
                dht: lookups.dht,
                relay,
            })
        }
        SwarmMode::Public => Ok(LookupOpts {
            mdns: true,
            dht: true,
            relay: RelayChoice::Pinned,
        }),
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::{
        LookupOpts, LookupSet, RelayChoice, RelaySelection, SwarmMode, resolve_lookups,
        validate_lookups,
    };

    fn lookups(mdns: bool, dht: bool, relay: RelaySelection) -> LookupSet {
        LookupSet { mdns, dht, relay }
    }

    fn url() -> iroh::RelayUrl {
        "https://relay.example".parse().unwrap()
    }

    fn ladder() -> Vec<iroh::RelayUrl> {
        vec![url()]
    }

    #[test]
    fn public_no_flags_enables_all_three() {
        let opts = resolve_lookups(SwarmMode::Public, LookupSet::default()).unwrap();
        assert!(opts.mdns && opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned, "no flags ⇒ pinned relay");
    }

    #[test]
    fn public_mdns_alone_disables_dht_and_relay() {
        let opts = resolve_lookups(
            SwarmMode::Public,
            lookups(true, false, RelaySelection::Unset),
        )
        .unwrap();
        assert!(opts.mdns && !opts.dht);
        assert_eq!(
            opts.relay,
            RelayChoice::Disabled,
            "--mdns alone ⇒ relay off"
        );
    }

    #[test]
    fn public_bare_relay_is_pinned_and_suppresses_lookups() {
        let opts = resolve_lookups(
            SwarmMode::Public,
            lookups(false, false, RelaySelection::Default),
        )
        .unwrap();
        assert!(!opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned);
    }

    #[test]
    fn public_valued_relay_is_custom_and_suppresses_lookups() {
        let opts = resolve_lookups(
            SwarmMode::Public,
            lookups(false, false, RelaySelection::Custom(ladder())),
        )
        .unwrap();
        assert!(!opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Custom(ladder()));
    }

    #[test]
    fn public_valued_relay_preserves_ladder_order() {
        let rung0: iroh::RelayUrl = "https://a.example".parse().unwrap();
        let rung1: iroh::RelayUrl = "https://b.example".parse().unwrap();
        let opts = resolve_lookups(
            SwarmMode::Public,
            lookups(
                false,
                false,
                RelaySelection::Custom(vec![rung0.clone(), rung1.clone()]),
            ),
        )
        .unwrap();
        assert_eq!(opts.relay, RelayChoice::Custom(vec![rung0, rung1]));
    }

    #[test]
    fn public_mdns_plus_relay_keeps_both() {
        let opts = resolve_lookups(
            SwarmMode::Public,
            lookups(true, false, RelaySelection::Default),
        )
        .unwrap();
        assert!(opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Pinned);
    }

    #[test]
    fn default_for_custom_relay_keeps_lookups_on() {
        // embed/MCP contract: a custom relay must not suppress mdns/dht.
        let opts = LookupOpts::default_for(SwarmMode::Public, ladder());
        assert!(opts.mdns && opts.dht);
        assert_eq!(opts.relay, RelayChoice::Custom(ladder()));
    }

    #[test]
    fn default_for_empty_ladder_is_pinned() {
        let opts = LookupOpts::default_for(SwarmMode::Public, Vec::new());
        assert_eq!(opts.relay, RelayChoice::Pinned);
    }

    #[test]
    fn private_no_flags_is_all_off() {
        let opts = resolve_lookups(SwarmMode::Private, LookupSet::default()).unwrap();
        assert!(!opts.mdns && !opts.dht);
        assert_eq!(opts.relay, RelayChoice::Disabled);
    }

    #[test]
    fn private_with_any_flag_is_rejected() {
        let cases = [
            lookups(true, false, RelaySelection::Unset),
            lookups(false, true, RelaySelection::Unset),
            lookups(false, false, RelaySelection::Default),
            lookups(false, false, RelaySelection::Custom(ladder())),
        ];
        for set in cases {
            let via_resolve = resolve_lookups(SwarmMode::Private, set.clone());
            assert!(via_resolve.is_err(), "resolve must reject: {set:?}");
            let error = validate_lookups(SwarmMode::Private, &set).unwrap_err();
            assert!(error.to_string().contains("--public"), "got: {error}");
        }
    }
}

#[cfg(test)]
mod directory_selection_tests {
    use super::{DEFAULT_DIRECTORY, DirectorySelection, SwarmMode, SwarmName, validate_advertise};

    #[test]
    fn unset_is_not_advertising() {
        let sel = DirectorySelection::Unset;
        assert!(!sel.is_set());
        assert!(sel.directory().is_none());
    }

    #[test]
    fn bare_resolves_to_default_directory() {
        let sel = DirectorySelection::Default;
        assert!(sel.is_set());
        assert_eq!(sel.directory().unwrap().as_str(), DEFAULT_DIRECTORY);
    }

    #[test]
    fn named_resolves_to_that_directory() {
        let sel = DirectorySelection::Named(SwarmName::new("gamedev").unwrap());
        assert_eq!(sel.directory().unwrap().as_str(), "gamedev");
    }

    #[test]
    fn advertise_requires_public() {
        // Private + advertising is rejected, naming the flag.
        let error =
            validate_advertise(SwarmMode::Private, &DirectorySelection::Default).unwrap_err();
        assert!(error.to_string().contains("--advertise"), "got: {error}");
        assert!(error.to_string().contains("--public"), "got: {error}");
        // Public + advertising, and private + not advertising, are fine.
        assert!(validate_advertise(SwarmMode::Public, &DirectorySelection::Default).is_ok());
        assert!(validate_advertise(SwarmMode::Private, &DirectorySelection::Unset).is_ok());
    }
}
