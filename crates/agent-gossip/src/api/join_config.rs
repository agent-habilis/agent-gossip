use habilis_network::protocol::{JoinTarget, LookupSet, MeshName, Nickname, TransportPolicy};
use habilis_network::util::tuning::GOSSIP_ACTIVE_VIEW_CAPACITY;

use crate::policy;

/// How to join a mesh.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JoinConfig {
    /// What to join: a mesh id, classified into a [`JoinTarget`] at the
    /// boundary (parse a string with [`str::parse`]). The name, lookups and
    /// transport policy are decoded from the id. (A shared *string* derives its own
    /// mesh — see the `topic` command — and is not a join target.)
    pub target: JoinTarget,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Password for a password-protected id. Verified locally against the
    /// id's verifier before any network; required iff the id carries one.
    pub password: Option<String>,
}

impl JoinConfig {
    /// A config for `target` with a random nickname and the default
    /// peer cap. Set [`JoinConfig::nickname`] / [`JoinConfig::max_peers`]
    /// afterwards to override. Build the [`JoinTarget`] by parsing a
    /// string (`"<hash>".parse()?`).
    #[must_use]
    pub fn new(target: JoinTarget) -> Self {
        Self {
            target,
            nickname: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: None,
        }
    }

    /// Use `nickname` instead of a random one.
    #[must_use]
    pub fn with_nickname(mut self, nickname: Nickname) -> Self {
        self.nickname = Some(nickname);
        self
    }

    /// Cap the direct peer connections at `max_peers`.
    #[must_use]
    pub fn with_max_peers(mut self, max_peers: usize) -> Self {
        self.max_peers = max_peers;
        self
    }

    /// Present `password` for a password-protected id.
    #[must_use]
    pub fn with_password(mut self, password: String) -> Self {
        self.password = Some(password);
        self
    }
}

/// How to join a **topic**: a public mesh derived deterministically from a
/// shared string. The name and (always-public) config are derived from the
/// string, so it is the only input — anyone passing the same string converges
/// on the same mesh.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TopicConfig {
    /// The shared string. Hashed into the mesh seed after trimming
    /// surrounding whitespace (never case-folded).
    pub string: String,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
}

impl TopicConfig {
    /// A config for `string` with a random nickname and the default peer cap.
    #[must_use]
    pub fn new(string: String) -> Self {
        Self {
            string,
            nickname: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
        }
    }

    /// Use `nickname` instead of a random one.
    #[must_use]
    pub fn with_nickname(mut self, nickname: Nickname) -> Self {
        self.nickname = Some(nickname);
        self
    }

    /// Cap the direct peer connections at `max_peers`.
    #[must_use]
    pub fn with_max_peers(mut self, max_peers: usize) -> Self {
        self.max_peers = max_peers;
        self
    }
}

/// How to create a new mesh. Built from validated domain types
/// ([`MeshName`], [`Nickname`], [`LookupSet`]); the iroh `RelayUrl`
/// stays hidden behind [`RelayLadder`](crate::RelayLadder) inside the
/// lookups, so the surface is iroh-free.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CreateConfig {
    /// The mesh name (validated): 1..=32 UTF-8 characters (any
    /// script/emoji), excluding control characters, whitespace, and any
    /// of `/ \ < > #`.
    pub name: MeshName,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// Granular lookup allowlist (`mdns`/`dht`/`relay`). Naming any one
    /// uses *only* those (relay defaults off); naming none makes a
    /// loopback-only mesh. Default [`LookupSet::default`] (all off, i.e.
    /// loopback). Mirrors the CLI `--lookup`/`--relay-url` flags.
    pub lookups: LookupSet,
    /// Which transports may carry mesh payload. [`CreateConfig::new`] takes
    /// `udp,webrtc,gossip`, with no multihop and no relay. Mirrors the CLI
    /// `--transport` flag; a list set here is literal.
    pub transport: TransportPolicy,
    /// List this mesh in a directory so discoverers can find it
    /// without its id. Requires a lookup that reaches other machines.
    /// Default `false`.
    pub advertise: bool,
    /// The directory to advertise into when `advertise` is set.
    /// `None` ⇒ the well-known `global` directory.
    pub directory: Option<MeshName>,
    /// Max direct peer connections before gossip relays the rest.
    pub max_peers: usize,
    /// Protect the mesh with a password: its verifier is baked into the
    /// minted id (joiners must present the password), and every derivation
    /// switches onto the Argon2id-stretched key.
    pub password: Option<String>,
}

impl CreateConfig {
    /// A private-network config for mesh `name` with a random
    /// nickname and the default peer cap. Set the other fields
    /// afterwards to override.
    #[must_use]
    pub fn new(name: MeshName) -> Self {
        Self {
            name,
            nickname: None,
            lookups: LookupSet::default(),
            transport: policy::app_policy(false),
            advertise: false,
            directory: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: None,
        }
    }

    /// Use `nickname` instead of a random one.
    #[must_use]
    pub fn with_nickname(mut self, nickname: Nickname) -> Self {
        self.nickname = Some(nickname);
        self
    }

    /// Cap the direct peer connections at `max_peers`.
    #[must_use]
    pub fn with_max_peers(mut self, max_peers: usize) -> Self {
        self.max_peers = max_peers;
        self
    }

    /// Protect the mesh with `password`.
    #[must_use]
    pub fn with_password(mut self, password: String) -> Self {
        self.password = Some(password);
        self
    }

    /// Use `lookups` as the lookup allowlist.
    #[must_use]
    pub fn with_lookups(mut self, lookups: LookupSet) -> Self {
        self.lookups = lookups;
        self
    }

    /// Use `transport` as the transport policy.
    #[must_use]
    pub fn with_transport(mut self, transport: TransportPolicy) -> Self {
        self.transport = transport;
        self
    }

    /// List the mesh in a directory so discoverers find it without its id.
    #[must_use]
    pub fn with_advertise(
        mut self,
        // true lists the mesh in the directory; false keeps it unlisted
        advertise: bool,
    ) -> Self {
        self.advertise = advertise;
        self
    }

    /// Advertise into `directory`; `None` means the `global` directory.
    #[must_use]
    pub fn with_directory(mut self, directory: Option<MeshName>) -> Self {
        self.directory = directory;
        self
    }
}
