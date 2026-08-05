use fofoca::protocol::JoinTarget;
use fofoca::protocol::Nickname;
use fofoca::protocol::{LookupSet, MeshName};
use fofoca::util::consts::GOSSIP_ACTIVE_VIEW_CAPACITY;

/// How to join a mesh.
#[derive(Debug, Clone)]
pub struct JoinConfig {
    /// What to join: a mesh id, classified into a [`JoinTarget`] at the
    /// boundary (parse a string with [`str::parse`]). The network mode and
    /// name are decoded from the id. (A shared *string* derives its own
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
}

/// How to join a **topic**: a public mesh derived deterministically from a
/// shared string. The name and (always-public) config are derived from the
/// string, so it is the only input — anyone passing the same string converges
/// on the same mesh.
#[derive(Debug, Clone)]
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
}

/// How to create a new mesh. Built from validated domain types
/// ([`MeshName`], [`Nickname`], [`LookupSet`]); the iroh `RelayUrl`
/// stays hidden behind [`RelayLadder`](crate::RelayLadder) inside the
/// lookups, so the surface is iroh-free.
#[derive(Debug, Clone)]
pub struct CreateConfig {
    /// The mesh name (validated): 1..=32 UTF-8 characters (any
    /// script/emoji), excluding control characters, whitespace, and any
    /// of `/ \ < > #`.
    pub name: MeshName,
    /// Local nickname. `None` mints a random `word-word` one.
    pub nickname: Option<Nickname>,
    /// `true` ⇒ the all-on lookup preset (mDNS + DHT + default relay
    /// ladder) when `lookups` names nothing; `false` ⇒ loopback only.
    /// Sugar over `lookups`, mirroring the CLI `--public`. Default `false`.
    pub public: bool,
    /// Granular lookup allowlist (`mdns`/`dht`/`relay`). Naming any one
    /// uses *only* those (relay defaults off); naming none falls back to
    /// `public`. Default [`LookupSet::default`] (all off). Mirrors the
    /// CLI `--mdns`/`--dht`/`--relay` flags.
    pub lookups: LookupSet,
    /// List this mesh in a directory so discoverers can find it
    /// without its id. Requires `public`. Default `false`.
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
            public: false,
            lookups: LookupSet::default(),
            advertise: false,
            directory: None,
            max_peers: GOSSIP_ACTIVE_VIEW_CAPACITY,
            password: None,
        }
    }
}
